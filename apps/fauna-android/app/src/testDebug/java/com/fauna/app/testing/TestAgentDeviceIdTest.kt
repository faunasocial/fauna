package com.fauna.app.testing

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.AppState
import com.fauna.app.core.FfiCryptoOps
import com.fauna.app.core.InstallSecretStore
import com.fauna.app.core.LogicalSecretStore
import com.fauna.app.core.MemoryBackend
import com.fauna.app.core.SecureStorage
import com.fauna.ffi.FfiAccountRegistry
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Observers read; only a session consumer resolves — through the real shared
 * registry over the host-built `libfauna_ffi`. The android twin of windows'
 * `SessionDeviceIdTests` (the observer half).
 *
 * [SecureStorage.resolveDeviceId] is a WRITE: the shared get-or-create persists
 * a derived id into the account's own slot. The moment-1 retirement
 * (`retire_superseded_provisionals`, `long-term-store.md` § Eager vs. lazy
 * migration at native boot) retires an identity the user walked away from
 * mid-wizard only while every per-actor slot but its secret is absent
 * (`AccountRegistry::is_provisional`). The e2e state snapshot is polled through
 * the whole wizard run, so a snapshot that resolved turned the abandoned
 * identity into a ghost switcher row — windows' journey caught it, and android published the same getter.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class TestAgentDeviceIdTest {

    private val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
    private val storage = SecureStorage(
        ApplicationProvider.getApplicationContext(),
        FfiCryptoOps(),
        registry,
        InstallSecretStore(LogicalSecretStore(MemoryBackend())),
    )

    /** The wizard's confirm step (moment 1). */
    private fun confirm(secretHex: String): String = registry.confirmIdentity(secretHex, false)

    /** A mid-wizard state publish: no session override, so the snapshot reads the registry. */
    private fun publishState(): JSONObject =
        TestAgent.serializeState(AppState(), storage, db = null).getJSONObject("session")

    @Test
    fun aStatePublishLeavesAnUnfinishedIdentityRetirableByTheNextConfirm() {
        confirm(SECRET_A)

        val published = publishState()
        assertSame("nothing has minted A's id yet", JSONObject.NULL, published.get("device_id"))

        val kept = confirm(SECRET_B)
        val rows = registry.list()
        assertEquals("the abandoned identity is retired, not a second switcher row", 1, rows.size)
        assertEquals(kept, rows[0].actorId)
    }

    @Test
    fun aStatePublishServesTheStoredSlotVerbatim() {
        registry.addAccount(SECRET_A, "https://a.example", STORED)

        assertEquals(STORED, publishState().getString("device_id"))
    }

    @Test
    fun theStoredReadIsPure() {
        confirm(SECRET_A)
        assertNull(storage.storedDeviceId)
        assertNull("a read wrote nothing", storage.storedDeviceId)
    }

    /**
     * A stored slot that is not a 32-byte-hex sync id (the launch harness seeds
     * `smoke-e2e-device`) is never served by the resolving reader: the shared
     * get-or-create derives a sync id and persists it over the slot — windows'
     * `SessionDeviceId.Resolve` shape.
     */
    @Test
    fun aResolveNeverServesANonSyncStoredSlotVerbatim() {
        registry.addAccount(SECRET_A, "https://a.example", STORED)

        val resolved = storage.resolveDeviceId()

        assertNotNull(resolved)
        assertTrue("a 64-hex sync id, not the stored slot", Regex("[0-9a-f]{64}").matches(resolved!!))
        assertEquals("the derived id is persisted into the slot", resolved, storage.storedDeviceId)
    }

    /**
     * The other side of the line, and why an observer may not cross it: a
     * resolve on the unfinished identity persists its id, and the row survives
     * the next confirm as the ghost.
     */
    @Test
    fun aResolveOfAnUnfinishedIdentityKeepsItPastTheNextConfirm() {
        confirm(SECRET_A)

        val resolved = storage.resolveDeviceId()
        assertNotNull(resolved)
        assertEquals(resolved, storage.storedDeviceId)

        confirm(SECRET_B)
        assertEquals(2, registry.list().size)
    }

    private companion object {
        const val SECRET_A = "0101010101010101010101010101010101010101010101010101010101010101"
        const val SECRET_B = "0202020202020202020202020202020202020202020202020202020202020202"
        const val STORED = "smoke-e2e-device"
    }
}
