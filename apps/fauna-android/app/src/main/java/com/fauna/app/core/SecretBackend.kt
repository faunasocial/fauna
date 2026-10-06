package com.fauna.app.core

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/** The app's credential file: identity, per-account slots, the registry index. */
internal const val SECURE_PREFS_FILE = "fauna_secure_prefs"

/**
 * The install-scoped file sign-out never wipes — the install device secret's
 * home ([InstallSecretStore]).
 */
internal const val INSTALL_PREFS_FILE = "fauna_install_prefs"

/**
 * Opens the app's single encrypted key/value file, `fauna_secure_prefs`.
 *
 * Android returns the SAME underlying `SharedPreferences` singleton for a given
 * file name within a process, so every `EncryptedSharedPreferences` wrapper
 * built here (same file, same master-key config, deterministic AES256_SIV key
 * encryption) reads and writes the identical rows. That is what lets
 * [SharedPrefsSecretBackend] (the multi-account registry's storage) and
 * [SecureStorage] (its install-local `auto_photo_backup` preference and the
 * sign-out reset of the whole file) share one store.
 *
 * [fileName] opens a different encrypted file under the same master key — the
 * one other file is [INSTALL_PREFS_FILE] (see [InstallSecretStore]).
 */
internal fun openSecurePrefs(
    context: Context,
    fileName: String = SECURE_PREFS_FILE,
): SharedPreferences {
    val masterKey = MasterKey.Builder(context)
        .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
        .build()
    return EncryptedSharedPreferences.create(
        context,
        fileName,
        masterKey,
        EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
        EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
    )
}

/**
 * A flat, raw key/value store keyed by the registry's logical key, verbatim.
 * The Android analogue of windows' `ISecretBackend`
 * (`FaunaApp.Core/Services/SecretBackend.cs`): [LogicalSecretStore] passes
 * each logical key straight through to it. Swappable so the same
 * `FfiSecretStore` seam can run over the real encrypted store in production or a
 * test/e2e backend.
 *
 * `set` must be durable before it returns: the CR-1 factory-reset claim code the
 * registry writes here through `mint_and_persist_pending_factory_reset` must
 * survive a SIGKILL microseconds later (`nest/common.md` Section Client-state
 * recoverability). The production backend uses `SharedPreferences.commit()`
 * (synchronous), never `apply()`, exactly as [SecureStorage] already does for
 * the same reason; the shared mint rail additionally proves durability by
 * read-back.
 */
interface SecretBackend {
    fun get(key: String): String?
    fun set(key: String, value: String)
    fun delete(key: String)
}

/**
 * Production [SecretBackend] over an [openSecurePrefs] store — the shared
 * `fauna_secure_prefs` by default. Every write commits synchronously (CR-1
 * durability; see the interface doc).
 */
class SharedPrefsSecretBackend(
    context: Context,
    fileName: String = SECURE_PREFS_FILE,
) : SecretBackend {
    private val prefs: SharedPreferences = openSecurePrefs(context, fileName)

    override fun get(key: String): String? = prefs.getString(key, null)

    override fun set(key: String, value: String) {
        // commit(), never apply(): the pending-factory-reset claim code must be
        // on disk before the registry's mint rail returns it.
        prefs.edit().putString(key, value).commit()
    }

    override fun delete(key: String) {
        prefs.edit().remove(key).commit()
    }
}
