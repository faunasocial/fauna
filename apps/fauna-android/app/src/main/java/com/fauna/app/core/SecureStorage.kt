package com.fauna.app.core

import android.content.Context
import android.content.SharedPreferences
import android.util.Log
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.FfiSessionMaterial
import dagger.hilt.android.qualifiers.ApplicationContext
import javax.inject.Inject
import javax.inject.Singleton

private const val TAG = "FaunaCredentialStore"

/**
 * The app-wide [SessionAccount] over the multi-account registry, plus the one
 * install-local preference that still lives beside the registry rows in the
 * encrypted `fauna_secure_prefs` file ([autoPhotoBackup]) and that file's
 * sign-out reset ([clear]).
 *
 * Holds no identity material: every [SessionAccount] property resolves the
 * registry's session account (`FfiAccountRegistry.sessionAccount` — on android,
 * the active account) and reads its `FfiSessionMaterial`, so there is no copy for
 * a switch or a sign-in to leave stale. The single-slot rows this class once
 * read and wrote (`secret_key` / `node_url` / `cached_*` and the three
 * wizard-resume slot helpers) are retired with the registry's downgrade mirror
 * (`long-term-store.md` § Downgrade mirror + abandoned-append recovery).
 */
@Singleton
class SecureStorage @Inject constructor(
    @ApplicationContext context: Context,
    // The device id's three collaborators (see `deviceIdFor` below): the actor
    // id from the identity's secret, the shared get-or-create, and the store the
    // install secret lives in.
    private val cryptoOps: CryptoOps,
    private val registry: FfiAccountRegistry,
    private val installSecretStore: InstallSecretStore,
) : SessionAccount {
    // The same encrypted `fauna_secure_prefs` file the multi-account registry's
    // SharedPrefsSecretBackend opens (openSecurePrefs). Opened on first use: only
    // the photo-backup toggle and [clear] touch it, so the identity reads above
    // stay constructible on a host JVM, which has no Android keystore.
    private val prefs: SharedPreferences by lazy { openSecurePrefs(context) }

    /** The session account's material, or null when no account is in session. */
    private fun material(): FfiSessionMaterial? = runCatching {
        registry.sessionAccount()?.let { registry.sessionMaterial(it) }
    }.getOrNull()

    override val secretHex: String?
        get() = material()?.secretHex?.ifEmpty { null }

    override val nestUrl: String?
        get() = material()?.nestUrl?.ifEmpty { null }

    /** The session account's device-id slot as it stands — a pure read. */
    override val storedDeviceId: String?
        get() = material()?.deviceId?.ifEmpty { null }

    /**
     * The session account's sync device id — lowercase hex, non-null in normal
     * operation. Resolved through [deviceIdFor] for the session account's own
     * secret: the account's persisted id when it is a 32-byte-hex sync id (any
     * other stored value is never served verbatim — the export replaces it), else
     * `derive_device_id(install_secret, actor_id)`, which the shared
     * get-or-create persists into the account's own slot. So a sign-out →
     * sign-in comes back to the SAME named `sync_devices` row
     * (`sync-agent-credentials.md` § Credential model, the 2026-09-20 ruling).
     *
     * One get-or-create serves the ~14 consumers across the workers, kicks and
     * VMs (each takes a `?: return` null branch), so no consumer has to know the
     * rule. It is a method, not a property, because it writes ([SessionAccount]'s
     * own doc says why that matters). Concurrent first calls are safe: the shared
     * mint is serialized in-process and derives from the one secret that read back.
     */
    override fun resolveDeviceId(): String? =
        material()?.let { deviceIdFor(it.secretHex) }

    override fun deviceIdFor(secretHex: String?): String? = DeviceIdSlot.resolve(secretHex, cryptoOps) { actor ->
        registry.deviceIdForActor(installSecretStore.store, actor)
    }

    override val handle: String?
        get() = material()?.handle

    override val domain: String?
        get() = material()?.domain

    override val tier: String?
        get() = material()?.tier

    var autoPhotoBackup: Boolean
        get() = prefs.getBoolean(KEY_AUTO_PHOTO_BACKUP, false)
        set(value) = bestEffort("auto_photo_backup") {
            prefs.edit().putBoolean(KEY_AUTO_PHOTO_BACKUP, value).commit()
        }

    fun clear() {
        bestEffort("clear") { prefs.edit().clear().commit() }
    }
    /**
     * Canonical pattern from Linux (tracked internally): persistence is
     * best-effort + log a structured warning. A failed save means the user
     * re-onboards next launch — same
     * outcome the wizard already handles. Crashing or silent-dropping (the
     * old shape) is worse: the user loses their secret AND can't tell why.
     *
     * Uses `commit()` rather than `apply()` so the boolean return reflects
     * whether the disk write actually succeeded; `apply()` is fire-and-forget
     * and would silently lose data without log.
     */
    private inline fun bestEffort(label: String, block: () -> Boolean) {
        try {
            if (!block()) {
                Log.w(TAG, "persist $label failed: SharedPreferences.commit returned false")
                ShellLog.w(TAG, "persist $label failed: SharedPreferences.commit returned false")
            }
        } catch (e: Throwable) {
            Log.w(TAG, "persist $label failed", e)
            ShellLog.w(TAG, "persist $label failed: ${e.message}")
        }
    }

    companion object {
        private const val KEY_AUTO_PHOTO_BACKUP = "auto_photo_backup"
    }
}
