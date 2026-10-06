package com.fauna.app.core

/**
 * An in-memory [SecretBackend], keyed by the registry's logical key verbatim —
 * the same key space [SharedPrefsSecretBackend] uses, minus the encrypted-prefs
 * plumbing that can't run headless. The Android twin of windows'
 * `MemoryBackend` in `AccountRegistryStoreTests`.
 *
 * Shared by every unit test that needs a real `FfiAccountRegistry` without a
 * device.
 */
internal class MemoryBackend(seed: Map<String, String> = emptyMap()) : SecretBackend {
    private val map = HashMap(seed)

    override fun get(key: String): String? = map[key]

    override fun set(key: String, value: String) {
        map[key] = value
    }

    override fun delete(key: String) {
        map.remove(key)
    }

    /** The wholesale reset sign-out runs after `clearAll` (`SecureStorage.clear()`). */
    fun clear() = map.clear()
}
