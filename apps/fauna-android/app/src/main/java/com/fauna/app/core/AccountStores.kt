package com.fauna.app.core

import android.content.Context
import androidx.room.Room
import com.fauna.app.data.db.FaunaDatabase
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.FfiPredecessorChain
import dagger.hilt.android.qualifiers.ApplicationContext
import java.io.File
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Owns this install's **account-scoped** stores — the client-side data that
 * belongs to one identity — and their lifecycle across sign-out.
 *
 * Authority: `docs/goal/architecture/apps/account-scoping.md`. Its taxonomy
 * puts every persisted datum in exactly one class; the paths this class hands
 * out are android's class-1/class-4 set, each inside an actor scope. Everything
 * else stays directly under its root, shared, and outside every actor scope the
 * erases drop — which is how android's install-scoped state keeps its class-2
 * disposition: the log dir (`FaunaApp.installLogging`) and the nest-identity
 * TOFU pin store (`FaunaApp.installNestIdentityPinStore`, keyed by host — a
 * property of the network path, not the person).
 *
 * Android has **two** state roots, so each is scoped with its own call: Room
 * resolves a bare database name under `databases/`, while every other store
 * lives under `filesDir`. The shared Rust mechanism takes one base per call.
 *
 * Both halves of the isolation contract live here: **placement** (every store
 * below resolves under `<root>/<actor-hex>/`) and **erasure** (§ Erasure follows
 * scope). Apple's [AccountStateDir] is the reference shape this mirrors —
 * `apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/AccountStateDir.swift` —
 * including its disposition that a session which cannot name its account still
 * runs, against a scope of its own: `<root>/-unresolved-`
 * ([UNRESOLVED_ACTOR_COMPONENT]), never the root itself.
 */
@Singleton
class AccountStores @Inject constructor(
    @ApplicationContext private val context: Context,
    private val registry: FfiAccountRegistry,
) {
    companion object {
        /** The Room database name; also its store name under the `databases/` root. */
        const val ROOM_DB_NAME = "fauna.db"

        /**
         * The conversations MLS store's name inside the scoped dir — the
         * cross-app target `account-scoping.md` § Serialized switching names
         * (`<state base>/<actor-id-hex>/mls.db`), which apple writes too. linux
         * and tui kept `mls_state.db` when they scoped — a pre-existing name
         * divergence noted in that doc's ledger, not one to spread here.
         */
        const val SCOPED_MLS_DB_NAME = "mls.db"

        /**
         * The scope a session that cannot name its account resolves under —
         * shared Rust's `UNRESOLVED_ACTOR_COMPONENT`, the component apple's and
         * windows' `AccountStateDir` take. Never 64 hex, so it can never collide
         * with a real actor's scope, and never empty, so it can never collapse
         * back onto the root.
         */
        const val UNRESOLVED_ACTOR_COMPONENT = "-unresolved-"

        const val P2P_DB_NAME = "p2p_contacts.db"

        const val SYNC_DIR = "sync"

        /**
         * The **W6 (account-data-plane.md § Workstreams) account-store container** — the per-user root this sandboxed
         * app hands the account runtime and the erases, a *sibling* of the
         * per-actor scopes rather than a child of one
         * (`docs/goal/architecture/account-data-plane.md` § The account store).
         *
         * ⚠ **Deliberately NOT `sync/`, though that is the desktop root's name
         * and the iOS twin's.** Those two resolve it inside a container of its
         * own (`~/.config/fauna/sync`; the iOS app-group container), where the
         * name is free. Android has exactly one `filesDir`, and `sync` is the
         * sync-engine store's name there ([SYNC_DIR], inside each actor scope,
         * which sits beside the container under that same `filesDir`). A
         * distinct name is what keeps the two from ever meaning each other.
         */
        const val ACCOUNT_STORE_DIR = "account-store"

        /**
         * The client-side backup **audit** loop's state file — this device's own
         * evidence about each destination's freshness/inclusion verdict
         * (`docs/goal/ui/backups.md` § Audit-alert surface). Every field is
         * re-derivable (losing it costs exactly one re-audit), so it is a
         * **class-4** replica — mirrors linux's `sync.rs::backup_state_dir`.
         */
        const val BACKUP_AUDIT_STATE_FILE = "backup-audit-state.json"
    }

    /** `filesDir` — the root every store but the Room database resolves under. */
    private val filesRoot: File
        get() = context.filesDir

    /** The `databases/` dir Room resolves a bare database name under. */
    private val dbRoot: File
        get() = context.getDatabasePath(ROOM_DB_NAME).parentFile
            ?: File(context.dataDir, "databases")

    private var db: FaunaDatabase? = null

    /** Closers other owners of account-scoped handles register (see [registerCloser]). */
    private val closers = linkedMapOf<String, () -> Unit>()

    /**
     * The active account's actor id (lowercase hex), or `null` when this install
     * has no identity yet (onboarding) or holds an index this build cannot read.
     *
     * The registry is the same ground truth the switch itself writes, so every
     * caller — app, worker, or the documents `ContentProvider` starting before
     * any Hilt injection — resolves the same account without an activation
     * handshake to miss.
     */
    fun activeActorHex(): String? = runCatching { registry.active() }.getOrNull()

    /**
     * [actorIdHex]'s retired owner `BackupKey`s off its succession chain,
     * empty for an identity that never succeeded (`sync-agent.md` §
     * Credential model → *Retired owner keys after an identity succession*).
     * Resolve ONCE post-auth and share across every consumer —
     * `ConversationsManagerHost.startConversationsSession`'s `__mls` re-seal
     * and any future sync-agent/label-custody leg — mirroring apple
     * `FaunaClient.makeSyncAgentProvisioner`'s `predecessorBackupKeys` and
     * tui `session.rs::succession_predecessor_backup_keys`.
     *
     * ⚠ Takes the actor explicitly — never [activeActorHex] internally. The
     * active pointer can disagree with the actor the session is actually
     * built for (an append-mode sign-in, or a switch racing
     * `ConversationsVM.rebuild`), and a wrong-actor list makes the `__mls`
     * re-seal silently do nothing. Callers pass the
     * actor derived from the same secret the session builds with — see
     * `ApiClient.ensureNestConnected`.
     */
    fun predecessorBackupKeys(actorIdHex: String): List<ByteArray> =
        runCatching { registry.predecessorBackupKeys(actorIdHex) }
            .onFailure {
                ShellLog.w("AccountStores", "predecessorBackupKeys resolve failed for $actorIdHex: ${it.message}")
            }
            .getOrDefault(emptyList())

    /**
     * [actorIdHex]'s attested predecessor ids (32 bytes each, nearest hop
     * first) — the Media listing judge's reader-side input, ruling (8)(b) of
     * `writer-signed-change-records.md`. Same actor discipline as
     * [predecessorBackupKeys].
     */
    fun attestedPredecessorActorIds(actorIdHex: String): List<ByteArray> =
        runCatching { registry.attestedPredecessorActorIds(actorIdHex) }
            .onFailure {
                ShellLog.w("AccountStores", "attestedPredecessorActorIds resolve failed for $actorIdHex: ${it.message}")
            }
            .getOrDefault(emptyList())

    /**
     * [actorIdHex]'s predecessors PAIRED with their retired keys off the
     * registry's one walk (`FfiAccountRegistry.predecessorChain`) — never a zip
     * of [attestedPredecessorActorIds] and [predecessorBackupKeys], whose
     * lengths can differ. Empty for an identity that never succeeded.
     */
    fun predecessorChain(actorIdHex: String): FfiPredecessorChain =
        runCatching { registry.predecessorChain(actorIdHex) }
            .onFailure {
                ShellLog.w("AccountStores", "predecessorChain resolve failed for $actorIdHex: ${it.message}")
            }
            .getOrDefault(FfiPredecessorChain(emptyList(), emptyList()))

    /**
     * Every identity [actorIdHex] succeeded from, as the registry records it
     * (hex, nearest hop first; empty for an identity that never succeeded) —
     * what the profile edit admits a stored base signed by someone else
     * against (`profile.md` § After an identity succession, the successor
     * RE-PUBLISHES). Unlike [predecessorBackupKeys] it lists rows whose secret
     * this device does not hold: admitting a base needs the succession link,
     * not the key.
     *
     * ⚠ Same actor discipline as [predecessorBackupKeys]: callers pass the
     * actor derived from the session's own secret (`ApiClient.sessionActorHex`).
     */
    /**
     * The account registry itself, for the one FFI door that RECORDS into it on
     * the app's behalf — the profile edit form's base load
     * (`FfiProfileClient.loadEditBase`), which proves and records a succession
     * link the base needs so [predecessorsOf] names it at the save.
     */
    val accountRegistry: FfiAccountRegistry get() = registry

    fun predecessorsOf(actorIdHex: String): List<String> =
        runCatching { registry.predecessorsOf(actorIdHex) }
            .onFailure {
                ShellLog.w("AccountStores", "predecessorsOf resolve failed for $actorIdHex: ${it.message}")
            }
            .getOrDefault(emptyList())

    /**
     * The active account's restorable last-known supervision snapshot, or
     * `null` when there is nothing to enforce (family-safety.md § Content
     * policy, clause 2). The guardian-less refusal — a slot whose last read
     * said unsupervised restores as no snapshot — lives in the shared FFI
     * getter, not here. Each enforcement store seeds itself from this at
     * construction, ahead of its first read landing.
     */
    fun supervisionSnapshot(): com.fauna.ffi.FfiSupervisionSnapshot? =
        activeActorHex()?.let { runCatching { registry.supervisionSnapshot(it) }.getOrNull() }

    /**
     * Clause 2's write: fold a **successful** `fauna.family.status` reply into
     * [actorIdHex]'s persisted snapshot. Only [ApiClient.familyStatus]'s
     * success path calls this — the one choke point every status reader goes
     * through — keyed on the actor whose session made the read, never the
     * active pointer (a mid-switch transient must not stamp the wrong
     * account's slot). Best-effort: a store hiccup must not fail the read
     * that produced a perfectly good reply.
     */
    fun persistSupervisionSnapshot(actorIdHex: String, status: com.fauna.ffi.FfiFamilyStatus) {
        runCatching { registry.persistSupervisionSnapshot(actorIdHex, status) }
            .onFailure { ShellLog.w("AccountStores", "supervision-snapshot persist failed: ${it.message}") }
    }

    /**
     * Register a drop for actor-scoped state this class does not itself own —
     * an open handle ([P2PManager]'s `FfiPeerDb`), a loaded cache, or a
     * background loop's cancellation seam. Keyed, so re-registering replaces the
     * entry rather than growing the list.
     *
     * **This is the INNER list of android's two-level canonical drop**
     * ([ActorScope], which owns the whole design note; `account-scoping.md`
     * § the in-memory corollary sanctions the two levels, "one list per
     * assembly, the outer calling the inner"). Nothing calls [closeOpenStores]
     * to effect an identity change directly — [ActorScope.dropActorScopedState]
     * is the one door, and it runs these closers first, while the nest client is
     * still up.
     *
     * ⚠ **Register here only what the outer list cannot name**: state held by an
     * activity-/ViewModel-scoped owner ([com.fauna.app.ui.viewmodel.SupervisedIndicatorVM],
     * the e2e session override on [AppState]), or by a Hilt `@Singleton` whose
     * construction is lazy — where naming it in [ActorScope] would *construct*
     * it, and run its `init` network fetches, on a teardown path. Everything
     * else belongs in the outer list, where it is statically greppable. The
     * silent failure mode a registry adds — a surface whose registration nobody
     * wrote — is exactly what left four android surfaces on no list at all until
     * 2026-08-27.
     */
    @Synchronized
    fun registerCloser(key: String, closer: () -> Unit) {
        closers[key] = closer
    }

    /** The active account's dir under `filesDir`. */
    private fun filesScope(): File = scope(filesRoot)

    /** The active account's dir under the Room `databases/` root. */
    private fun dbScope(): File = scope(dbRoot)

    /**
     * `<root>/<actor-hex>/`, created — or `<root>/-unresolved-/` when there is no
     * nameable active account or the derivation refuses the id: a session that
     * cannot name its account still runs, against a scope of its own, never the
     * root and never another account's scope (apple's and windows'
     * `AccountStateDir.scopeDir`).
     */
    private fun scope(root: File): File {
        val dir = activeActorHex()
            ?.let { hex -> runCatching { accountRoot(root, hex) }.getOrNull() }
            ?: File(root, UNRESOLVED_ACTOR_COMPONENT)
        dir.mkdirs()
        return dir
    }

    /** The conversations MLS group store (`ApiClient.ensureNestConnected`). */
    fun conversationsMlsDbPath(): String = File(filesScope(), SCOPED_MLS_DB_NAME).absolutePath

    /**
     * [actorIdHex]'s conversations MLS store, its scope dir **created** — the
     * same file [conversationsMlsDbPath] names once that actor is active. The
     * succession ceremony's successor resolver ([SuccessorStorePath]); a
     * malformed hex lands under [UNRESOLVED_ACTOR_COMPONENT], never the root.
     */
    fun mlsDbPathFor(actorIdHex: String): String {
        val dir = scopeFor(filesRoot, actorIdHex)
        dir.mkdirs()
        return File(dir, SCOPED_MLS_DB_NAME).absolutePath
    }

    /**
     * [mlsDbPathFor] **without creating anything** — the retired identity's
     * resolver ([RetiredIdentityStorePath]). The sweep retry turns on whether
     * that store already exists, so asking must never make one.
     */
    fun pureMlsDbPathFor(actorIdHex: String): String =
        File(scopeFor(filesRoot, actorIdHex), SCOPED_MLS_DB_NAME).absolutePath

    private fun scopeFor(root: File, actorIdHex: String): File =
        runCatching { accountRoot(root, actorIdHex) }.getOrNull()
            ?: File(root, UNRESOLVED_ACTOR_COMPONENT)

    /** The P2P peer store (`P2PManager.db`). */
    fun p2pContactsDbPath(): String = File(filesScope(), P2P_DB_NAME).absolutePath

    /**
     * The backup-audit loop's state file (`fauna_ffi::backup_audit_run_pass` /
     * `backup_audit_observe`'s `state_path`) — this device's own audit
     * evidence, actor-scoped so an account switch cannot let one account's
     * observation high-water suppress another's freshness failures
     * (`docs/goal/ui/backups.md` § Audit-alert surface).
     */
    fun backupAuditStatePath(): String = File(filesScope(), BACKUP_AUDIT_STATE_FILE).absolutePath

    /**
     * The **unscoped** base the client-device custodian store resolves under —
     * the one path on this class that is deliberately *not* pre-scoped.
     *
     * `build_custodian_host` takes a shell's flat per-user base and derives the
     * actor-scoped location itself, through the shared
     * `custodian_store_root(base, actor_hex)` → `<base>/<actor-hex>/
     * backup-custodian/` (`libs/fauna-sync-engine/src/custodian_store.rs`), so
     * the layout and the account scoping cannot drift per platform. Handing it
     * [filesScope] instead would scope the path **twice**, and the second hex
     * would come from a different source than the first: `filesScope` reads the
     * account registry, while Rust derives the actor from the owner secret the
     * host was built with. A store keyed on a hex the source never lists looks
     * perfectly correct on disk and can restore nothing.
     *
     * Erasure still follows scope: the store lands *inside*
     * `<filesDir>/<actor-hex>/`, which `accountStateEraseScope` (one account)
     * and the sign-out finish (`signOutFinish`) both drop wholesale.
     */
    fun custodianStoreBaseDir(): String = filesRoot.also { it.mkdirs() }.absolutePath

    /**
     * The W6 account-store container this app hosts its account runtime from —
     * `<filesDir>/account-store`, see [ACCOUNT_STORE_DIR] for why not `sync/`.
     *
     * ⚠ **The same value must reach `startAccountRuntime` AND both erases.**
     * The shared Rust resolves `Some(dir) -> StoreRoot::at(dir)`, `None ->
     * StoreRoot::platform()`, and on android `platform()` is
     * `$HOME/.config/fauna/sync` — unwritable or plain wrong inside the sandbox.
     * An erase that resolved that while the runtime opened this one would sweep
     * a directory the store was never in and leave the real store behind, with
     * its writer key destroyed by the credential wipe alongside: every later
     * sign-in refused with "account store belongs to a different writer", the
     * app silently on the blob rail for good. That is the failure measured on
     * tui 2026-08-18; routing every caller through this one accessor is what
     * makes the two impossible to disagree.
     *
     * Not per-actor: `StoreRoot::store_dir(actor_hex)` scopes underneath it
     * itself, so passing a per-actor dir here would double-scope.
     */
    fun accountStoreContainerDir(): String =
        File(filesRoot, ACCOUNT_STORE_DIR).also { it.mkdirs() }.absolutePath

    /**
     * The sync engine's state dir — the one `FfiSyncEngineHost` key both the
     * photo ingress and the watched-directory ingress pass, so they keep sharing
     * a single device-local state store *within* an account.
     */
    fun syncStateDir(): String = File(filesScope(), SYNC_DIR).also { it.mkdirs() }.absolutePath

    /**
     * [actorIdHex]'s sync state dir **without creating anything** — what the
     * on-demand toggle's succession carry probes a predecessor's scope with
     * (`FfiOnDemandSource.prefsStore`); the active actor's is [syncStateDir].
     */
    fun syncStateDirFor(actorIdHex: String): File = File(scopeFor(filesRoot, actorIdHex), SYNC_DIR)

    /**
     * The Room database file. Room resolves a bare name under `databases/`; an
     * absolute name is used verbatim (with its parent created), which is how the
     * account's subdir is reached without a second Room configuration surface.
     */
    fun roomDbPath(): String = File(dbScope(), ROOM_DB_NAME).absolutePath

    /**
     * The Room database for the active account, opened on first use.
     *
     * Deliberately **not** a Hilt `@Singleton` binding: the account-scoped
     * handle has to be closeable and re-openable, and a cached `@Singleton`
     * would hand every consumer built after a sign-out the same closed
     * instance. Hilt's `provideDatabase` delegates here instead, so a
     * consumer constructed after a sign-out ([finishSignOut]) gets a fresh, empty store.
     */
    @Synchronized
    fun database(): FaunaDatabase {
        db?.let { return it }
        val built = Room.databaseBuilder(context, FaunaDatabase::class.java, roomDbPath())
            .addMigrations(*FaunaDatabase.MIGRATIONS)
            .build()
        db = built
        FaunaDatabase.setInstance(built)
        return built
    }

    /**
     * Close every open account-scoped handle — this class's Room database plus
     * whatever other owners registered via [registerCloser].
     *
     * Two callers, two reasons. **Before an erase:** deleting a SQLite file out
     * from under an open connection leaves the connection serving a deleted
     * inode, so the erase would look successful while the old rows stayed
     * queryable. **On a switch:** a handle opened under account A points at A's
     * files no matter who is active now, so it must not survive into B's session.
     * Anything still holding a closed handle then fails loudly instead of
     * silently serving the previous account — the fail-closed direction the
     * isolation contract asks for (`account-scoping.md` § The scoping taxonomy,
     * the switch/sign-out isolation contract).
     */
    @Synchronized
    fun closeOpenStores() {
        db?.close()
        db = null
        FaunaDatabase.clearInstance()
        // Kept registered, not cleared: these are idempotent invalidators, and a
        // registrant that only registers once (a @Singleton dropping a cached
        // account-specific value in its `init`) must still be invalidated by the
        // NEXT switch, not just the first.
        for ((key, closer) in closers) {
            runCatching { closer() }
                .onFailure { ShellLog.w("AccountStores", "closing $key failed: ${it.message}") }
        }
    }

    /**
     * The switch teardown: end the outgoing account's stores so the incoming one
     * opens its own.
     *
     * Deliberately no erase: the outgoing account stays on this install and keeps
     * its data. Only the *handles* end.
     */
    @Synchronized
    fun endActiveAccountSession() {
        closeOpenStores()
    }

    /**
     * **The sign-out's decision**, written before anything else — after the
     * serving question passed, before the runtime's stop is awaited
     * (`account-scoping.md` § Erasure follows scope → *The sign-out record is on
     * all 7 apps*, decision 1): `sign-out-record.json` under `filesDir` names
     * every account the registry lists plus [servedActor] and owes the
     * credential wipe, so a process killed anywhere after this is finished by
     * the next launch ([finishSignOut]). The account-index reset passes `null`
     * over an index it cannot read: a wipe owed with no accounts. Returns the
     * accounts recorded, for the gesture's own [finishSignOut].
     */
    fun recordSignOut(servedActor: String?): List<String> =
        com.fauna.ffi.signOutRecordBegin(registry, filesRoot.absolutePath, servedActor)

    /**
     * **The one routine** the sign-out's tail, the cold launch and Remove Again
     * run — shared Rust's finish over both roots and the account-store container:
     * the serving question, the sync agent dismissed (every [caller] but the
     * gesture, whose own stop already did it), the wipe (the registry's own store,
     * then [host]'s `secureStorage.clear()`), each recorded account's scopes and
     * shared-namespace slots, the residue re-sweep, the residue record written
     * before the sign-out record clears (decisions 2–6). [caller] is which of
     * the three runs this is — while the registry names somebody only Remove
     * Again re-sweeps the residue record, its paths only (decision 3). [written]
     * is the gesture's [recordSignOut] answer, `null` elsewhere.
     *
     * Closes the open account-scoped handles first — deleting a SQLite file out
     * from under an open connection leaves it serving a deleted inode — but only
     * when a record is pending or this is the gesture: at a launch with nothing
     * to finish, the identity closers must not blank the live account's state.
     * Blocking file I/O (and, but for the gesture, the agent's budget): call it
     * off the main thread.
     */
    @Synchronized
    fun finishSignOut(
        host: com.fauna.ffi.FfiSignOutHost,
        caller: com.fauna.ffi.FfiFinishCaller,
        written: List<String>?,
    ): com.fauna.ffi.FfiSignOutFinish {
        if (written != null || com.fauna.ffi.signOutRecordPending(filesRoot.absolutePath)) {
            closeOpenStores()
        }
        val outcome = com.fauna.ffi.signOutFinish(
            registry,
            listOf(filesRoot.absolutePath, dbRoot.absolutePath),
            accountStoreContainerDir(),
            // `ownLock = null`: android admits one instance per app and takes
            // no raw lock, so there is no reflection of its own to put down.
            null,
            host,
            caller,
            written,
        )
        if (outcome.refused) {
            ShellLog.w(
                "AccountStores",
                "sign-out finish REFUSED — another instance serves an account it reaches " +
                    "(fails closed: ${outcome.failsClosed})",
            )
        } else if (outcome.residue != null) {
            ShellLog.w("AccountStores", "sign-out finish left this user's data on the device")
        }
        return outcome
    }

    /**
     * Remove ONE account from this install — the account-switcher's "remove
     * this account" gesture, WHOLE, through the shared FFI door
     * (`com.fauna.ffi.removeAccountGesture`): refused while this window or a
     * sibling serves it; else its registry entry and slots go, this phone is
     * retired from the account's nest as a sign-out retires it, and its scoped
     * stores are erased under both roots (`account-scoping.md` § Erasure follows
     * scope → *Removing an account retires this machine from it, as a sign-out
     * does*). Returns the refusal to paint, `null` once removed; throws when the
     * registry refuses the removal (nothing else ran then).
     *
     * The nest half is best-effort under the shared budget, so this may take up
     * to that long on a nest that does not answer — call it off the main thread.
     *
     * ⚠ No [closeOpenStores]. Every caller targets a non-active row
     * ([AccountSettingsVM.removeAccount], offered only for a non-active
     * account), and the door refuses the active one (`servingHere`), so the
     * active session's handles are never the ones being erased.
     * [closeOpenStores] runs EVERY registered identity closer, not just this
     * actor's: `ContentPolicyStore`, `ScreenTimeStore` and
     * `SupervisedIndicatorVM` each blank their state unconditionally and only
     * re-seed it when the active actor actually changed
     * (family-client-enforcement.md:98-99), so running them for a non-active
     * removal blanked the ACTIVE ward's guardian rules, bedtime lock and
     * indicator.
     */
    suspend fun removeAccount(actorIdHex: String): com.fauna.ffi.FfiEraseRemoveBlocked? =
        com.fauna.ffi.removeAccountGesture(
            registry,
            actorIdHex,
            listOf(filesRoot.absolutePath, dbRoot.absolutePath),
            // The same container, paired with the same exclusion, that
            // `ApiClient.startAccountRuntime` hands the live runtime.
            com.fauna.ffi.FfiStoreContainer(
                dir = accountStoreContainerDir(),
                exclusion = CloudBackupPosture.exclusion(),
            ),
            null,
            activeActorHex(),
        )

    /**
     * Where one account's stores live under `root`: `<root>/<actor-id-hex>/`,
     * resolved through the shared Rust derivation so this app, its workers, and
     * every other app agree on the layout.
     */
    fun accountRoot(root: File, actorIdHex: String): File =
        File(com.fauna.ffi.accountStateDir(root.absolutePath, actorIdHex))
}
