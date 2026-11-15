package com.fauna.app.p2pshare

/**
 * The account's **group-ring mirror seat** (`group-set.md` § Build design —
 * the group set, decision 12) — **the built half** of the `p2p-share` glue:
 * every build type but `storeSafe` and `kids` compiles this file; those take
 * the inert `src/noP2pShare/` twin, signature-identical and naming no ring FFI
 * symbol (their bindings export no `installGroupRingMirror`).
 *
 * The shell's one duty: name the sync state dir this process's capability
 * hosts of the account are built with (`syncStateDir()`), once, beside the
 * retained-key custody install. The runtime writes `group-rings.sealed` there
 * from then on — at the store's mount, after every store change and at the
 * seat — so no app call at an assembly or a walk can be forgotten, and the
 * provider's in-process host reads the ring a group set's keys come from.
 * Idempotent: seating again replaces the dir and triggers a pass.
 */
object GroupRingMirrorSeat {
    /** Seat [stateDir] for [actorId]. Throws when the actor id is malformed. */
    fun install(actorId: ByteArray, stateDir: String) {
        com.fauna.ffi.installGroupRingMirror(actorId, stateDir)
    }
}
