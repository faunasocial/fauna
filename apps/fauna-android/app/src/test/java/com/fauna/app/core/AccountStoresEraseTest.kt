package com.fauna.app.core

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiAccountRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.File

/**
 * The sign-out half of the isolation contract
 * (`account-scoping.md` § Erasure follows scope): sign-out erases every
 * account-scoped CONTENT store, not just the credential namespace, while
 * install-scoped state survives.
 *
 * Before this, `AccountSettingsVM.signOut` cleared only the registry + secure
 * storage, so the whole Room content set, the conversations MLS db, the P2P
 * store, and the backup/sync state stayed on disk and readable — the gap the
 * doc's android ledger row records.
 *
 * FFI-touching (the erase runs through shared Rust) -> runs green via
 * `just android-host-test`, which puts the locally built `libfauna_ffi.so` on
 * the JVM's `jna.library.path`. The shared-Rust twin is
 * `fauna_account_store::db::tests::erase_all_account_scopes_drops_every_scope_but_not_install_state`.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class AccountStoresEraseTest {

    /**
     * Both roots start empty, so a scope left by a previous test cannot decide
     * this one's outcome — order-independence has to be explicit.
     */
    @Before
    fun clearBothStateRoots() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        ctx.filesDir.deleteRecursively()
        ctx.getDatabasePath(AccountStores.ROOM_DB_NAME).parentFile?.deleteRecursively()
        ctx.filesDir.mkdirs()
        com.fauna.app.data.db.FaunaDatabase.clearInstance()
    }

    /**
     * An [AccountStores] over the REAL shared registry, with [secretHex]
     * registered — the first account added is the active one.
     */
    private fun stores(secretHex: String? = null): AccountStores {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
        secretHex?.let { registry.addAccount(it, null, null) }
        return AccountStores(ApplicationProvider.getApplicationContext(), registry)
    }

    /** The actor id the shared derivation gives [SECRET_A] / [SECRET_B]. */
    private fun actorOf(secretHex: String): String =
        HexUtil.bytesToHex(com.fauna.ffi.actorIdFromSecret(HexUtil.hexToBytes(secretHex)))

    private fun seed(dir: File, rel: String, body: String = "x"): File {
        val f = File(dir, rel)
        f.parentFile?.mkdirs()
        f.writeText(body)
        return f
    }

    @Test
    fun signOutErasesEveryAccountScopedStoreUnderBothRootsButKeepsInstallState() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        val filesRoot = ctx.filesDir
        val dbRoot = ctx.getDatabasePath(AccountStores.ROOM_DB_NAME).parentFile!!
        dbRoot.mkdirs()

        // Account-scoped content under filesDir, in two accounts' scopes…
        val actorHex = "aa11".repeat(16)
        val mls = seed(File(filesRoot, actorHex), AccountStores.SCOPED_MLS_DB_NAME)
        val p2p = seed(File(filesRoot, actorHex), AccountStores.P2P_DB_NAME)
        val sync = seed(File(filesRoot, actorHex), "${AccountStores.SYNC_DIR}/device.db")
        val other = seed(File(filesRoot, "bb22".repeat(16)), AccountStores.SCOPED_MLS_DB_NAME)

        // ...and the Room database under its own root.
        val room = seed(File(dbRoot, actorHex), AccountStores.ROOM_DB_NAME)

        // Install-scoped state (class 2) — describes the device, not the person.
        // Stand-ins: what matters is that an entry NOT named in the store lists
        // survives, which is the property the log dir and the host-keyed nest
        // TOFU pin store rely on (their real filenames are chosen in shared Rust).
        val log = seed(filesRoot, "logs/app.log")
        val pins = seed(filesRoot, "some-install-scoped-file.json")

        stores().eraseAllAccounts()

        assertFalse("MLS store survived sign-out", mls.exists())
        assertFalse("P2P store survived sign-out", p2p.exists())
        assertFalse("sync state survived sign-out", sync.exists())
        assertFalse("another account's scope survived sign-out", other.exists())
        assertFalse("Room database survived sign-out", room.exists())

        assertTrue("install-scoped log was erased", log.exists())
        assertTrue("install-scoped nest pins were erased", pins.exists())
    }

    /**
     * A sign-out that left nothing behind says **nothing** — *"0 items were left
     * behind"* is reassurance by vacuity, refused here as at every other seat
     * (`account-scoping.md` § Erasure follows scope → *A clean sweep says
     * nothing*).
     */
    @Test
    fun aCleanSignOutOwesNoLineAtAll() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        seed(File(ctx.filesDir, "aa11".repeat(16)), AccountStores.SCOPED_MLS_DB_NAME)

        val s = stores()
        val sweep = s.eraseAllAccounts()

        assertTrue("nothing should have survived: ${sweep.survivors}", sweep.survivors.isEmpty())
        assertFalse("a clean sweep owes no work", sweep.residue.owesWork)
        assertEquals(null, s.recordResidue(sweep, CLEAN_CREDENTIALS))
        assertFalse(
            "a clean sweep keeps no record for the next launch to re-check",
            residueRecord(ctx).exists(),
        )
    }

    /**
     * ⚠ **The defect exists for**: a sign-out whose
     * erase could not remove everything must not be indistinguishable from one
     * that could. Android turned that outcome into a `ShellLog.w` and painted a
     * clean "Signed out" over a device that still held the user's readable MLS
     * and account stores.
     *
     * The count reaches the user's line; the **path does not** — it belongs in
     * the log (`account-scoping.md` § Erasure follows scope → *the count goes to
     * the user; the paths go to the log*).
     *
     * Fault injection is a **read-only scope directory**, so the file inside it
     * cannot be unlinked. Not a held handle: POSIX `unlink` removes an open file
     * happily, which is exactly why this defect was invisible everywhere but
     * Windows for as long as it existed. Skips itself loudly under a process
     * that writes through a read-only dir, rather than passing for the wrong
     * reason.
     */
    @Test
    fun aScopeThatWillNotGoReachesTheUserByCountAndTheLogByPath() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        val actorHex = "aa11".repeat(16)
        val scope = File(ctx.filesDir, actorHex)
        val pinned = seed(scope, AccountStores.SCOPED_MLS_DB_NAME)
        // A second, ordinary scope: one root that will not go must not spare the
        // rest of the sweep.
        val ordinary = seed(File(ctx.filesDir, "bb22".repeat(16)), AccountStores.SCOPED_MLS_DB_NAME)
        assertTrue("precondition: the scope is read-only", scope.setWritable(false))

        if (pinned.delete()) {
            scope.setWritable(true)
            println("skipping: this process can write through a read-only dir (root?)")
            return
        }

        val s = stores()
        val sweep = s.eraseAllAccounts()
        scope.setWritable(true)

        assertTrue("precondition: the read-only scope is what makes this survive", pinned.exists())
        assertFalse("the other scope must still have been erased", ordinary.exists())

        assertEquals("one path survived: ${sweep.survivors}", 1, sweep.survivors.size)
        assertTrue(
            "the log gets the PATH: ${sweep.survivors}",
            sweep.survivors.single().contains(actorHex),
        )
        assertTrue("the user is owed a line", sweep.residue.owesWork)

        val residue = s.recordResidue(sweep, CLEAN_CREDENTIALS)
        assertNotNull("a survivor owes the user a line", residue)
        val line = resolveLocalized(ctx, residue!!.line())!!
        assertTrue("the line names how many items survived, got $line", line.contains("1"))
        assertTrue("the line names the Remove Again control beside it, got $line", line.contains("Remove Again"))
        assertFalse(
            "the survivor PATH belongs in the log, never on the user's line: $line",
            line.contains(actorHex),
        )
        assertTrue("the residue is recorded so it outlives the process", residueRecord(ctx).exists())
    }

    /**
     * Remove Again (`sign-out-residue-retry-button`) runs the shared re-sweep
     * over exactly what the sign-out recorded: once the fault is gone the scope
     * goes, the view closes (`null`) and the record is deleted
     * (`account-scoping.md` § Erasure follows scope → *the residue surface*).
     * A retry while the fault holds keeps the view up.
     */
    @Test
    fun removeAgainFinishesTheEraseOnceTheScopeWillGo() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        val scope = File(ctx.filesDir, "aa11".repeat(16))
        val pinned = seed(scope, AccountStores.SCOPED_MLS_DB_NAME)
        assertTrue("precondition: the scope is read-only", scope.setWritable(false))
        if (pinned.delete()) {
            scope.setWritable(true)
            println("skipping: this process can write through a read-only dir (root?)")
            return
        }
        val s = stores()
        val residue = s.recordResidue(s.eraseAllAccounts(), CLEAN_CREDENTIALS)
        assertNotNull("precondition: the erase left the scope", residue)

        val eraser = CountingEraser()
        val stillThere = s.retryResidue(residue!!, eraser)
        assertNotNull("the fault still holds, so the view stays up", stillThere)

        scope.setWritable(true)
        val after = s.retryResidue(stillThere!!, eraser)

        assertEquals("a clean re-sweep closes the view", null, after)
        assertFalse("the recorded scope is gone", scope.exists())
        assertFalse("and so is the record", residueRecord(ctx).exists())
        assertEquals("a clean credential half is never re-erased", 0, eraser.calls)
    }

    /**
     * The record outlives the process: a signed-out launch finds it, re-sweeps
     * it FIRST and paints nothing when the residue now goes — while a launch
     * with an account in the registry leaves it alone for the next signed-out
     * one (`account-scoping.md` § Erasure follows scope → *the residue
     * surface*, "a signed-out launch re-sweeps it silently first").
     */
    @Test
    fun aSignedOutLaunchReSweepsTheRecordSilentlyAndASignedInOneLeavesIt() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        val scope = File(ctx.filesDir, "aa11".repeat(16))
        val pinned = seed(scope, AccountStores.SCOPED_MLS_DB_NAME)
        assertTrue("precondition: the scope is read-only", scope.setWritable(false))
        if (pinned.delete()) {
            scope.setWritable(true)
            println("skipping: this process can write through a read-only dir (root?)")
            return
        }
        stores().let { it.recordResidue(it.eraseAllAccounts(), CLEAN_CREDENTIALS) }
        scope.setWritable(true)

        assertEquals(
            "a signed-in launch is not the user the residue was reported to",
            null,
            stores(SECRET_A).recheckResidueAtLaunch(CountingEraser()),
        )
        assertTrue("so nothing was swept", pinned.exists())
        assertTrue("and the record waits", residueRecord(ctx).exists())

        assertEquals(
            "a residue that now goes is finished without a word",
            null,
            stores().recheckResidueAtLaunch(CountingEraser()),
        )
        assertFalse("the recorded scope is gone", scope.exists())
        assertFalse("and so is the record", residueRecord(ctx).exists())
    }

    /** The seat's credential erase, counted — a clean credential half never runs it. */
    private class CountingEraser : com.fauna.ffi.FfiResidueCredentialEraser {
        var calls = 0
        override fun eraseCredentials(): com.fauna.ffi.FfiCredentialSweep {
            calls++
            return CLEAN_CREDENTIALS
        }
    }

    private fun residueRecord(ctx: android.content.Context): File =
        File(ctx.filesDir, "sign-out-residue.json")

    /**
     * The reopen guarantee behind dropping the Hilt `@Singleton` on the database
     * binding: after an erase, the next consumer gets a NEW handle, and the static
     * accessor the documents provider / test agent read is re-pointed at it. A
     * cached `@Singleton` would hand every later consumer the closed instance
     * instead — that is the whole reason `provideDatabase` delegates here.
     *
     * ⚠ Deliberately asserts handle *identity*, never a query. Robolectric cannot
     * open real SQLite on an ARM64 host — its native runtime has no Linux aarch64
     * build, so `DefaultNativeRuntimeLoader.ensureLoaded` throws `AssertionError:
     * The Robolectric native runtime is not supported on Linux (aarch64)`. That is
     * a property of the development machine, not a bug in the code under test, and
     * it applies to every ARM64 development machine — so do NOT "strengthen" this
     * into a real read. Room opens its file lazily, which is what makes handle
     * identity observable without one; row-level assertions belong in the
     * emulator-backed end-to-end suite.
     */
    @Test
    fun anEraseYieldsAFreshHandleAndRepointsTheStaticAccessor() {
        val s = stores()
        val first = s.database()
        assertNotNull(first)

        s.eraseAllAccounts()

        val second = s.database()
        assertNotNull(second)
        assertTrue("erase must yield a NEW handle, not the closed one", first !== second)
        assertTrue(
            "the static accessor must follow the live handle",
            com.fauna.app.data.db.FaunaDatabase.getInstance() === second,
        )
    }

    // ------------------------------------------------------------------
    // Per-account PLACEMENT (`account-scoping.md` Section Serialized switching)
    // ------------------------------------------------------------------

    /**
     * Every account-scoped store resolves under `<root>/<actor-hex>/`, across
     * BOTH of android's state roots — Room resolves a bare name under
     * `databases/`, everything else under `filesDir`, so a single scoped root
     * would silently leave the whole Room content set flat.
     */
    @Test
    fun everyAccountScopedStoreResolvesUnderTheActiveActorsSubdirOfItsOwnRoot() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        val s = stores(SECRET_A)
        val actor = actorOf(SECRET_A)

        val filesScope = File(ctx.filesDir, actor)
        val dbScope = File(ctx.getDatabasePath(AccountStores.ROOM_DB_NAME).parentFile!!, actor)

        assertEquals(
            File(filesScope, AccountStores.SCOPED_MLS_DB_NAME).absolutePath,
            s.conversationsMlsDbPath(),
        )
        assertEquals(
            File(filesScope, AccountStores.P2P_DB_NAME).absolutePath,
            s.p2pContactsDbPath(),
        )
        assertEquals(
            File(filesScope, AccountStores.BACKUP_AUDIT_STATE_FILE).absolutePath,
            s.backupAuditStatePath(),
        )
        assertEquals(File(filesScope, AccountStores.SYNC_DIR).absolutePath, s.syncStateDir())
        assertEquals(File(dbScope, AccountStores.ROOM_DB_NAME).absolutePath, s.roomDbPath())
    }

    /**
     * Two accounts get two disjoint store sets — the property the whole contract
     * reduces to. Without it a switch renders the previous account's rows.
     */
    @Test
    fun twoAccountsResolveToDisjointStorePaths() {
        val a = stores(SECRET_A)
        val b = stores(SECRET_B)

        assertTrue(
            "both accounts resolved the SAME MLS store — no isolation at all",
            a.conversationsMlsDbPath() != b.conversationsMlsDbPath(),
        )
        assertTrue("Room database is shared across accounts", a.roomDbPath() != b.roomDbPath())
        assertTrue("sync state is shared across accounts", a.syncStateDir() != b.syncStateDir())
        assertTrue(
            "backup-audit state is shared across accounts",
            a.backupAuditStatePath() != b.backupAuditStatePath(),
        )
    }

    /**
     * A session that cannot name its account still runs, against a scope of its
     * own — `<root>/-unresolved-/` under BOTH roots, never the root itself and
     * never an actor's scope (apple's and windows' `AccountStateDir`, shared
     * Rust's `actor_state_dir_or_unresolved`).
     */
    @Test
    fun noActiveAccountResolvesUnderTheUnresolvedScope() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        val s = stores(secretHex = null)
        val filesScope = File(ctx.filesDir, AccountStores.UNRESOLVED_ACTOR_COMPONENT)
        val dbScope = File(
            ctx.getDatabasePath(AccountStores.ROOM_DB_NAME).parentFile!!,
            AccountStores.UNRESOLVED_ACTOR_COMPONENT,
        )

        assertEquals(
            File(filesScope, AccountStores.SCOPED_MLS_DB_NAME).absolutePath,
            s.conversationsMlsDbPath(),
        )
        assertEquals(File(filesScope, AccountStores.P2P_DB_NAME).absolutePath, s.p2pContactsDbPath())
        assertEquals(File(filesScope, AccountStores.SYNC_DIR).absolutePath, s.syncStateDir())
        assertEquals(File(dbScope, AccountStores.ROOM_DB_NAME).absolutePath, s.roomDbPath())
    }

    /**
     * The flat pre-scoping layout is retired, not adopted (`version-compatibility.md`
     * § Dimension 2, the fourth ratified exception): a store sitting directly
     * under `filesDir` in its old flat name reaches no account's scope.
     */
    @Test
    fun aFlatStoreAtTheRootIsNeverAdopted() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        seed(ctx.filesDir, "conversations-mls.db")
        seed(ctx.filesDir, "${AccountStores.SYNC_DIR}/folder-map.json", "{}")

        val s = stores(SECRET_A)

        assertFalse(
            "a flat MLS store was carried into the account's scope",
            File(s.conversationsMlsDbPath()).exists(),
        )
        assertFalse(
            "flat sync state was carried into the account's scope",
            File(s.syncStateDir(), "folder-map.json").exists(),
        )
    }

    /**
     * The switch teardown: every handle opened under the outgoing account is
     * closed, and registered invalidators fire on EVERY switch, not just the
     * first (the reason [AccountStores.closeOpenStores] keeps its registrations).
     */
    @Test
    fun endingAnAccountSessionClosesHandlesAndFiresRegisteredInvalidatorsEachTime() {
        val s = stores(SECRET_A)
        var invalidations = 0
        s.registerCloser("probe") { invalidations++ }

        val first = s.database()
        s.endActiveAccountSession()
        assertEquals("the switch must invalidate registered caches", 1, invalidations)
        assertTrue(
            "the outgoing account's database handle survived the switch",
            s.database() !== first,
        )

        s.endActiveAccountSession()
        assertEquals("a SECOND switch must invalidate too", 2, invalidations)
    }

    /**
     * The W6 (account-data-plane.md § Workstreams) account-store container is a
     * sibling of the per-actor scopes, not a child of one, and its name is
     * distinct from the per-actor sync-engine store's (`sync`, which is what the
     * desktop root and the iOS twin are called) — the name pin is what stops a
     * later session from "tidying" the container back to `sync` for symmetry
     * with iOS.
     */
    @Test
    fun theAccountStoreContainerIsASiblingOfTheScopesWithANameOfItsOwn() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        val container = File(stores(SECRET_A).accountStoreContainerDir())

        assertEquals(
            "the container must sit directly under filesDir — StoreRoot.store_dir " +
                "scopes per actor underneath it, so a per-actor container would double-scope",
            ctx.filesDir.canonicalFile,
            container.parentFile!!.canonicalFile,
        )
        assertTrue("the accessor must create the container", container.isDirectory)

        assertEquals("account-store", container.name)
        assertFalse(
            "the account-store container must not share the sync-engine store's name",
            container.name == AccountStores.SYNC_DIR,
        )
    }

    /**
     * A sign-out must sweep the **W6 account-store container** too, not just the
     * two roots that predate it.
     *
     * This is the android half of the success criteria, and the failure it
     * guards is the one `account-data-plane.md` § Implementation status today →
     * *Built — W3 the android host* calls silent: the container is a SIBLING of
     * `filesDir`'s per-actor scopes, so the pre-container erase missed it
     * entirely — and because the credential namespace swept alongside holds the
     * store's writer key, the stranded store then refuses every later sign-in
     * ("account store belongs to a different writer") and the app runs with no
     * account runtime, for good. Nothing fails loudly at any point.
     *
     * The Rust twin (`fauna-ffi`'s `a_sandboxed_host_erases_under_the_container_
     * it_hosts_from`) proves the shared sweep reaches a container it is GIVEN.
     * What only android can prove is that android actually gives it one — that
     * [AccountStores.eraseAllAccounts] passes [AccountStores.accountStoreContainerDir]
     * rather than letting it default to the unreachable platform root.
     */
    @Test
    fun signOutAlsoSweepsTheAccountStoreContainer() {
        val ctx = ApplicationProvider.getApplicationContext<android.app.Application>()
        val container = File(stores(SECRET_A).accountStoreContainerDir())
        val actorHex = actorOf(SECRET_A)

        // One account's store under the container, in the layout
        // `StoreRoot::store_dir` resolves: <container>/<actor-hex>/…
        val store = seed(File(container, actorHex), "account-store/store.db")
        // …and a second account's, so the sweep is shown to be all-scopes rather
        // than only the active one.
        val other = seed(File(container, actorOf(SECRET_B)), "account-store/store.db")
        assertTrue("seed did not land", store.exists() && other.exists())

        stores(SECRET_A).eraseAllAccounts()

        assertFalse(
            "the active account's W6 store survived sign-out — the container is a " +
                "SIBLING of the filesDir scopes, so an erase that does not receive it " +
                "sweeps neither, and the stranded store refuses every later sign-in",
            store.exists(),
        )
        assertFalse("another account's W6 store survived sign-out", other.exists())
    }

    private companion object {
        // Two valid 32-byte identity secrets (hex) — two distinct actors.
        const val SECRET_A =
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
        const val SECRET_B =
            "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f"

        /** A credential erase that left nothing readable. */
        val CLEAN_CREDENTIALS =
            com.fauna.ffi.FfiCredentialSweep(survivors = emptyList(), wipeFailed = false)
    }
}
