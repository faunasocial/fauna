package com.fauna.app.core

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiAccountRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import java.io.File

/**
 * **The custodian store base is the UNSCOPED root — scoping it here would scope
 * it twice, from two different sources.**
 *
 * `build_custodian_host` takes a shell's flat per-user base and derives the
 * store location itself, through the shared
 * `custodian_store_root(base, actor_hex)` → `<base>/<actor-hex>/custodian-store/`
 * (`libs/fauna-sync-engine/src/custodian_store.rs`). Every other path on
 * [AccountStores] is pre-scoped, so passing the obvious one is the natural
 * mistake — and the two hexes would not even come from the same place:
 * [AccountStores]'s scoping reads the account registry, while Rust derives the
 * actor from the owner secret the host was built with.
 *
 * The failure is silent and total: a store keyed on a hex the source never lists
 * looks perfectly correct on disk — right layout, real sealed blobs, growing
 * every pass — and can restore nothing, because its paths name an actor the
 * source has never heard of. It is the same drift the shared `CustodianHost`
 * assembly exists to prevent between the desktop and mobile arms; this test is
 * the android shell's half of it.
 *
 * FFI-touching (the registry is shared Rust) → runs green via
 * `just android-host-test`, like [AccountStoresEraseTest].
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class CustodianStoreBaseTest {

    /** A real actor, registered (and so active) in the registry [stores] builds. */
    private val secretHex = "11".repeat(32)

    @Before
    fun clearFilesRoot() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        ctx.filesDir.deleteRecursively()
        ctx.filesDir.mkdirs()
    }

    private fun stores(): AccountStores = AccountStores(
        ApplicationProvider.getApplicationContext(),
        FfiAccountRegistry(LogicalSecretStore(MemoryBackend())).also {
            it.addAccount(secretHex, null, null)
        },
    )

    private fun actorHex(): String =
        HexUtil.bytesToHex(com.fauna.ffi.actorIdFromSecret(HexUtil.hexToBytes(secretHex)))

    /**
     * The mutation this refuses is `filesScope()` — the one-word change that
     * makes every other accessor on this class correct. With an account ACTIVE
     * (so the two differ at all), the base must still be the flat root.
     */
    @Test
    fun theBaseIsTheFlatRootEvenWithAnAccountActive() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        val base = stores().custodianStoreBaseDir()

        assertEquals(
            "the custodian base must be the unscoped filesDir — shared Rust adds the " +
                "actor level, and adding it here too would key the store on a hex the " +
                "source never lists",
            ctx.filesDir.absolutePath,
            base,
        )
        assertFalse(
            "the base must not already carry an actor hex (got $base)",
            base.contains(actorHex()),
        )
    }

    /**
     * The flow assertion: the base is exactly **one level above** the dir the
     * account's other stores live in — which is precisely the level
     * `custodian_store_root` will add. Stated against a sibling accessor rather
     * than a literal, so it keeps holding if the scoped layout ever moves.
     */
    @Test
    fun sharedRustsOneAddedLevelLandsBesideThisAccountsOtherStores() {
        val stores = stores()
        val base = File(stores.custodianStoreBaseDir())
        // A scoped store, to name where this account's dir actually is.
        val scoped = File(stores.syncStateDir()).parentFile

        assertEquals(
            "custodian_store_root(base, actor) must resolve inside this account's scope; " +
                "if these disagree the store is keyed on the wrong actor",
            scoped!!.absolutePath,
            stores.accountRoot(base, actorHex()).absolutePath,
        )
    }
}
