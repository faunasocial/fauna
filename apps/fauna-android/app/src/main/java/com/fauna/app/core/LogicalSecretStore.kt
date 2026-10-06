package com.fauna.app.core

import com.fauna.ffi.FfiSecretStore

/**
 * Android's implementation of the shared `FfiSecretStore` callback interface —
 * the platform's ONLY foreign seam into `fauna-client-accounts`
 * (`FfiAccountRegistry` + the shared `RegistryLaunchPersistence`). The Android
 * twin of windows' `FaunaApp.Core/Services/LogicalSecretStore.cs`.
 *
 * Every logical key (`fauna/index`, `fauna/{actor}/...`, `install/...`) is
 * stored verbatim under its own name in the [SecretBackend]. All the
 * account-index and per-actor namespacing lives in shared Rust
 * (`libs/fauna-client-accounts`); android contributes only this pass-through
 * (priority #2). The pre-registry single-identity rows it once remapped onto
 * android's own native key names are retired (`long-term-store.md`
 * § Downgrade mirror + abandoned-append recovery).
 */
class LogicalSecretStore(private val backend: SecretBackend) : FfiSecretStore {
    override fun get(key: String): String? = backend.get(key)

    override fun set(key: String, value: String) = backend.set(key, value)

    override fun delete(key: String) = backend.delete(key)
}
