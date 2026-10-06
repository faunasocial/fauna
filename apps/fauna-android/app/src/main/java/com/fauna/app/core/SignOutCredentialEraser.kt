package com.fauna.app.core

import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.FfiCredentialSweep
import com.fauna.ffi.FfiResidueCredentialEraser

/**
 * Android's credential erase — the ONE sequence the sign-out, the
 * account-index reset and the residue retry all run, so the retry re-runs
 * exactly what the sign-out ran (`account-scoping.md` § Erasure follows scope →
 * *the residue surface*, "the seat's own credential erase").
 *
 * `clearAll` deletes every per-actor slot and the index and reads back what it
 * deleted; `secureStorage.clear()` is the platform store's own wholesale reset;
 * `reverifyErase` re-asks after it, so a key that reset took is not reported.
 * Fold after the LAST wipe — reading back any earlier reports credentials the
 * reset already removed.
 *
 * Handed to shared Rust as the residue retry's [FfiResidueCredentialEraser],
 * which reads the RECORDED keys back on top of this answer (the registry is
 * empty by then, so its own read-back cannot name them).
 */
class SignOutCredentialEraser(
    private val registry: FfiAccountRegistry,
    private val secureStorage: SecureStorage,
) : FfiResidueCredentialEraser {
    override fun eraseCredentials(): FfiCredentialSweep {
        val erased = registry.clearAll()
        secureStorage.clear()
        return registry.reverifyErase(erased)
    }
}
