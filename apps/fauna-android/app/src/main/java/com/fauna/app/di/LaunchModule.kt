package com.fauna.app.di

import android.content.Context
import com.fauna.app.core.FileSecretBackend
import com.fauna.app.core.INSTALL_PREFS_FILE
import com.fauna.app.core.InstallSecretStore
import com.fauna.app.core.LaunchObserverImpl
import com.fauna.app.core.LogicalSecretStore
import com.fauna.app.core.SecretBackend
import com.fauna.app.core.SharedPrefsSecretBackend
import com.fauna.app.testing.TestAgent
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.FfiSecretStore
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.components.SingletonComponent
import uniffi.fauna_launch_machine.LaunchMachine
import uniffi.fauna_launch_machine.LaunchPersistence
import javax.inject.Singleton

/**
 * Hilt bindings for the shared-Rust launch flow, over the multi-account
 * registry (`long-term-store.md` § Multi-account evolution; the CR-3 seam
 * collapse, `nest/common.md` § Client-state recoverability).
 *
 * The chain — Android's only launch-persistence path, no second hand-rolled one:
 *
 *   SecretBackend (fauna_secure_prefs)
 *     -> FfiSecretStore (LogicalSecretStore: every logical key verbatim)
 *       -> FfiAccountRegistry (shared: index, per-actor slots)
 *         -> LaunchPersistence (shared RegistryLaunchPersistence over the
 *            ACTIVE account) -> LaunchMachine
 *
 * The registry is the only store (`long-term-store.md` § Downgrade mirror +
 * abandoned-append recovery): launch routes on the registry's active account
 * alone, and `SecureStorage` — the app-wide [com.fauna.app.core.SessionAccount]
 * — reads that same account's session material, so nothing needs a boot-time
 * mirror or migration.
 */
@Module
@InstallIn(SingletonComponent::class)
object LaunchModule {

    @Provides
    @Singleton
    fun provideSecretBackend(@ApplicationContext context: Context): SecretBackend {
        // FAUNA_E2E_CREDENTIAL_FILE (TestAgent.credentialFilePath, set from
        // MainActivity.onCreate before setContent()) selects the file backend
        // for e2e — the android twin of windows' FAUNA_E2E_CREDENTIAL_DIR
        // check (CredentialStore.cs::Build). Lets a test pre-seed a whole
        // multi-account registry with no app code, same as every other app.
        val credentialFile = TestAgent.credentialFilePath
        return if (credentialFile != null) {
            FileSecretBackend(java.io.File(credentialFile))
        } else {
            SharedPrefsSecretBackend(context)
        }
    }

    /**
     * The ONE `FfiSecretStore` this app builds — the registry's seam, and,
     * lent to shared Rust here, the store its credential slots persist in.
     *
     * Android has no Rust keyring arm: `fauna-credential-store` drives the
     * desktop stores itself and drops every write on a phone, so without the
     * lend the W3 (account-data-plane.md § Workstreams) account runtime's writer key could never read back and
     * the assembly refused on every real device — it only ever assembled
     * under the e2e file backend (`account-data-plane.md` § Implementation
     * status today → *Built — W3 the apple host*, the android twin). Lending
     * the same store the registry rides keeps the writer key
     * (`fauna-account-store/<actor>`, namespace-prefixed by Rust) in the one
     * `EncryptedSharedPreferences` beside the identity it was minted for,
     * under `allowBackup=false` like everything else there
     * (`android.md` § Secret Storage). Installed at provide time — the
     * registry is built for launch routing, long before any sign-in — and
     * process-global, last install wins (the pin-store shape). Mirrors
     * apple's `FaunaAccounts.installPlatformCredentialStore()`.
     */
    @Provides
    @Singleton
    fun provideSecretStore(backend: SecretBackend): FfiSecretStore =
        LogicalSecretStore(backend).also { store ->
            try {
                com.fauna.ffi.installPlatformCredentialStore(store)
            } catch (e: Throwable) {
                // Robolectric cannot load the native .so — same escape hatch as
                // the pin-store install in FaunaApp.onCreate.
                android.util.Log.w("LaunchModule", "platform credential store install failed", e)
            }
        }

    @Provides
    @Singleton
    fun provideAccountRegistry(store: FfiSecretStore): FfiAccountRegistry =
        FfiAccountRegistry(store)

    /**
     * The install device secret's store ([InstallSecretStore]) — a second
     * encrypted file sign-out's `SecureStorage.clear()` does not reach. Under
     * e2e it is a file beside the credential file, so a launch's fresh dirs
     * model a fresh machine, as they do for every other credential.
     */
    @Provides
    @Singleton
    fun provideInstallSecretStore(@ApplicationContext context: Context): InstallSecretStore {
        val credentialFile = TestAgent.credentialFilePath
        val backend = if (credentialFile != null) {
            FileSecretBackend(java.io.File(credentialFile).resolveSibling("install-secrets.json"))
        } else {
            SharedPrefsSecretBackend(context, INSTALL_PREFS_FILE)
        }
        return InstallSecretStore(LogicalSecretStore(backend))
    }

    @Provides
    @Singleton
    fun provideLaunchPersistence(registry: FfiAccountRegistry): LaunchPersistence =
        registry.launchPersistence()

    @Provides
    @Singleton
    fun provideLaunchMachine(
        observer: LaunchObserverImpl,
        persistence: LaunchPersistence,
    ): LaunchMachine {
        val machine = LaunchMachine(observer, persistence)
        observer.attach(machine)
        return machine
    }
}
