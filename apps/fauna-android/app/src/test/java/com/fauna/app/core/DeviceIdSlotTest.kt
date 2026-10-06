package com.fauna.app.core

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.FfiSecretStore
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Android's sync device id ([DeviceIdSlot], as [SecureStorage.deviceIdFor]
 * wires it) through the REAL shared get-or-create — `FfiAccountRegistry.
 * deviceIdForActor` over the host-built `libfauna_ffi` — with the credential
 * store and the install store as two separate backends, the production shape
 * (`fauna_secure_prefs` and `fauna_install_prefs`, see [InstallSecretStore]).
 *
 * The ruling it pins (`sync-agent-credentials.md` § Credential model, the
 * 2026-09-20 ruling): android registered a NEW named `sync_devices` row at
 * every sign-in that followed a sign-out, because sign-out wiped the only
 * copy of a random id. The shared-Rust twin is
 * `fauna-client-accounts::device_id::tests`. Robolectric only for
 * `android.util.Log`; every store here is in memory.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class DeviceIdSlotTest {

    private val credentials = MemoryBackend()
    private val installBackend = MemoryBackend()
    private val registry = FfiAccountRegistry(LogicalSecretStore(credentials))
    private val install = LogicalSecretStore(installBackend)

    private fun deviceIdFor(secretHex: String?, installStore: FfiSecretStore = install) =
        DeviceIdSlot.resolve(secretHex, FfiCryptoOps()) { actor ->
            registry.deviceIdForActor(installStore, actor)
        }

    /** The sign-in the wizard's confirm step commits (moment 1). Returns the actor id. */
    private fun signIn(secretHex: String): String = registry.confirmIdentity(secretHex, false)

    /** `AccountSettingsVM.signOut`'s erase: the registry sweep, then the store reset. */
    private fun signOut() {
        registry.clearAll()
        credentials.clear()
    }

    /** The whole point: the sign-in after a sign-out comes back to its own row. */
    @Test
    fun aSignOutThenSignInComesBackToTheSameDeviceId() {
        signIn(SECRET_A)
        val first = deviceIdFor(SECRET_A)
        assertNotNull("a signed-in account must have a device id", first)
        assertEquals("the id is 32 bytes of lowercase hex", 64, first!!.length)

        signOut()
        assertTrue("sign-out erased the account and its slots", registry.list().isEmpty())
        signIn(SECRET_A)

        assertEquals(first, deviceIdFor(SECRET_A))
    }

    /** The install secret lives where the sign-out's store reset cannot reach. */
    @Test
    fun theInstallSecretSurvivesTheSignOutReset() {
        signIn(SECRET_A)
        deviceIdFor(SECRET_A)
        signOut()
        assertNotNull(installBackend.get("install/device_secret"))
    }

    /** Two accounts on one phone get ids no nest can link. */
    @Test
    fun twoAccountsNeverShareADeviceId() {
        signIn(SECRET_A)
        val a = deviceIdFor(SECRET_A)
        registry.addAccount(SECRET_B, null, null)
        assertNotEquals(a, deviceIdFor(SECRET_B))
    }

    /**
     * The append wizard holds the NEW identity's secret while the active
     * account keeps its own device id: resolving for the new identity must not
     * hand it that id (`completeAddAccount` did, until the derivation), and
     * must not disturb the active account's.
     */
    @Test
    fun anIdentityMidAppendNeverInheritsTheActiveAccountsId() {
        registry.addAccount(SECRET_A, "https://a.example", A_DEVICE)

        assertNotEquals(A_DEVICE, deviceIdFor(SECRET_B))
        assertEquals(A_DEVICE, deviceIdFor(SECRET_A))
    }

    /** The resolved id lands in the account's own per-actor slot, which the session material reads. */
    @Test
    fun theResolvedIdLandsInTheAccountsOwnSlot() {
        val actor = signIn(SECRET_A)
        val id = deviceIdFor(SECRET_A)
        assertNotNull(id)
        assertEquals(id, registry.sessionMaterial(actor)?.deviceId)
    }

    @Test
    fun noIdentityResolvesNull() {
        assertNull(deviceIdFor(null))
        assertNull(deviceIdFor(""))
    }

    /** An install secret that does not persist yields null — never an unstable id, never a throw. */
    @Test
    fun anInstallStoreThatDropsWritesResolvesNull() {
        signIn(SECRET_A)
        val dropping = object : FfiSecretStore {
            override fun get(key: String): String? = null
            override fun set(key: String, value: String) {}
            override fun delete(key: String) {}
        }
        assertNull(deviceIdFor(SECRET_A, dropping))
    }

    private companion object {
        const val SECRET_A = "0101010101010101010101010101010101010101010101010101010101010101"
        const val SECRET_B = "0202020202020202020202020202020202020202020202020202020202020202"

        /** The active account's sync-shaped id (only a 32-byte hex id outranks the derivation). */
        const val A_DEVICE = "c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3"
    }
}
