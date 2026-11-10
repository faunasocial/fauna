package com.fauna.app.core

import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.FfiCredentialSweep
import com.fauna.ffi.FfiResidueCredentialEraser
import com.fauna.ffi.FfiSignOutHost

/**
 * Android's own credential acts — handed to shared Rust by the sign-out
 * record's finish (`com.fauna.ffi.signOutFinish`, the one routine the sign-out's
 * tail, the launch and Remove Again run — `account-scoping.md` § Erasure follows
 * scope → *The sign-out record is on all 7 apps*) as its [FfiSignOutHost].
 *
 * - [wipeOwnStore] — the finish's wipe: shared Rust has already run the
 *   registry's own wipe (`wipe_own`: the index and this app's per-actor slots,
 *   never the shared account-store namespace, whose slots ride with each
 *   account's scopes); this is the platform store's own wholesale reset,
 *   `secureStorage.clear()`, which shared Rust reads back after.
 * - [eraseCredentials] — the residue retry's credential erase: `clearAll`
 *   deletes every per-actor slot and the index and reads back what it deleted;
 *   `secureStorage.clear()`; `reverifyErase` re-asks after it, so a key that
 *   reset took is not reported. Shared Rust reads the RECORDED keys back on top
 *   of this answer (the registry is empty by then).
 *
 * Also the [FfiResidueCredentialEraser] the residue face's older exports take.
 */
class SignOutCredentialEraser(
    private val registry: FfiAccountRegistry,
    private val secureStorage: SecureStorage,
) : FfiResidueCredentialEraser, FfiSignOutHost {
    override fun wipeOwnStore(): Boolean {
        // Best-effort and logged inside `clear()`; the read-back shared Rust
        // runs after this is the witness of what the reset left.
        secureStorage.clear()
        return true
    }

    override fun eraseCredentials(): FfiCredentialSweep {
        val erased = registry.clearAll()
        secureStorage.clear()
        return registry.reverifyErase(erased)
    }
}
