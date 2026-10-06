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
     * custodian-store/` (`libs/fauna-sync-engine/src/custodian_store.rs`), so
     * the layout and the account scoping cannot drift per platform. Handing it
     * [filesScope] instead would scope the path **twice**, and the second hex
     * would come from a different source than the first: `filesScope` reads the
     * account registry, while Rust derives the actor from the owner secret the
     * host was built with. A store keyed on a hex the source never lists looks
     * perfectly correct on disk and can restore nothing.
     *
     * Erasure still follows scope: the store lands *inside*
     * `<filesDir>/<actor-hex>/`, which `accountStateEraseScope` (one account)
     * and `accountStateEraseAllScopes` (sign-out) both drop wholesale.
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
     * consumer constructed after [eraseAllAccounts] gets a fresh, empty store.
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
     * The all-accounts erase behind sign-out (`account-scoping.md` § Erasure
     * follows scope, extending `long-term-store.md` § Cleanup contract from the
     * credential namespace to the content stores).
     *
     * Drops every per-account scope under both roots. Install-scoped state
     * survives by construction — it is not an actor scope. Best-effort per
     * root: a failure to erase one root must not skip the other, and sign-out
     * itself must still complete (the credentials are already gone by then).
     *
     * **Returns what would NOT go, folded across both roots into the ONE answer
     * the user is owed about this device.** Proceeding must not be
     * indistinguishable from succeeding: a sweep with survivors means the user's
     * readable data is still here, and the caller owes them a line
     * ([recordResidue]). Dropping this on the floor *is* the defect — it is what
     * android did until 2026-09-09, and what tui did on the sweep it reaches by
     * another route until that was fixed the same way.
     *
     * The fold is shared Rust's (`eraseSweepFold`), not a sum taken here:
     * android is the one host whose app base is two directories, and adding up
     * survivor counts is precisely where a seat re-derives "is the device
     * clean?" in a fourth language.
     */
    @Synchronized
    fun eraseAllAccounts(): com.fauna.ffi.FfiEraseSweep {
        closeOpenStores()
        return com.fauna.ffi.eraseSweepFold(
            listOf(
                eraseRoot(filesRoot),
                eraseRoot(dbRoot),
            )
        )
    }

    /**
     * Turn what a sign-out's erase left behind into the reports it owes
     * (`account-scoping.md` § Erasure follows scope → *the residue surface*):
     * the survivor **paths** to the log, the **record** to install-scoped state
     * (`sign-out-residue.json` under `filesDir`, so it outlives this process),
     * and the residue the `sign-out-residue` view on `identity_choice` paints.
     *
     * `null` is the clean outcome and the only one — *"0 items were left
     * behind"* is reassurance by vacuity — and it keeps no record on disk. The
     * record, its line and the decision of what is clean are all shared Rust's
     * (`signOutResidueRecord`); android only logs and hands the result on. Twin
     * of tui's and linux's `account_scope::record_residue`.
     *
     * `credentials` is the credential erase's read-back
     * ([SignOutCredentialEraser]), so this is built after that erase — both
     * halves of the sign-out, or the line answers for half of one.
     */
    fun recordResidue(
        sweep: com.fauna.ffi.FfiEraseSweep,
        credentials: com.fauna.ffi.FfiCredentialSweep,
    ): com.fauna.ffi.FfiSignOutResidue? {
        if (sweep.survivors.isNotEmpty()) {
            ShellLog.w(
                "AccountStores",
                "${sweep.survivors.size} path(s) SURVIVED the erase and still hold " +
                    "this user's data: ${sweep.survivors.joinToString(", ")}",
            )
        }
        if (credentials.survivors.isNotEmpty() || credentials.wipeFailed) {
            ShellLog.w(
                "AccountStores",
                "sign-in credentials SURVIVED the erase (wipe failed: ${credentials.wipeFailed}); " +
                    "still readable: ${credentials.survivors.joinToString(", ")}",
            )
        }
        return com.fauna.ffi.signOutResidueRecord(filesRoot.absolutePath, sweep, credentials)
    }

    /**
     * Remove Again (`sign-out-residue-retry-button`): the shared re-sweep over
     * exactly what [residue] recorded, behind the sign-out's own serving-lock
     * question. `null` when the device is now clean — the view closes.
     * Blocking file I/O: call it off the main thread.
     */
    fun retryResidue(
        residue: com.fauna.ffi.FfiSignOutResidue,
        eraser: com.fauna.ffi.FfiResidueCredentialEraser,
    ): com.fauna.ffi.FfiSignOutResidue? =
        // `ownLock = null`: android admits one instance per app and takes no
        // raw lock, so there is no reflection of its own to put down.
        residue.retry(registry, filesRoot.absolutePath, accountStoreContainerDir(), null, eraser)

    /**
     * The signed-out launch's silent re-check: a record a previous sign-out left
     * is re-swept FIRST, and a residue comes back only if something is still
     * left. Shared Rust skips it while the registry holds an account — a
     * signed-in launch is not the user the residue was reported to. Blocking
     * file I/O: call it off the main thread.
     */
    fun recheckResidueAtLaunch(
        eraser: com.fauna.ffi.FfiResidueCredentialEraser,
    ): com.fauna.ffi.FfiSignOutResidue? =
        com.fauna.ffi.signOutResidueRecheckAtLaunch(
            registry, filesRoot.absolutePath, accountStoreContainerDir(), null, eraser,
        )

    /**
     * Erase ONE account's scoped stores — the account-switcher's "remove this
     * account from this install" affordance (`account-scoping.md` § Erasure
     * follows scope: *removing one account erases that actor's stores*). Touches
     * nothing outside that actor's scoped dirs, under either root.
     *
     * ⚠ Only [closeOpenStores] when the erased actor IS the active one. Every
     * caller targets a non-active row ([AccountSettingsVM.removeAccount],
     * offered only for a non-active account) — "removeAccount targets a
     * non-active actor" and "erases no live engine"
     * (`account-scoping.md:248-251`) — so the active session's handles are
     * never the ones being erased. [closeOpenStores] runs EVERY registered
     * identity closer, not just this actor's: `ContentPolicyStore`,
     * `ScreenTimeStore` and `SupervisedIndicatorVM` each blank their state
     * unconditionally and only re-seed it when the active actor actually
     * changed (family-client-enforcement.md:98-99). Since erasing a
     * non-active account never moves the active pointer, running the closers
     * anyway blanked the ACTIVE ward's guardian floor, bedtime lock and
     * indicator on every non-active removal — the same shape as the debug
     * `TestAgent` reset/logout same-actor drop, but reachable from ordinary
     * UI.
     */
    @Synchronized
    fun eraseAccount(actorIdHex: String) {
        if (actorIdHex == activeActorHex()) closeOpenStores()
        for (root in listOf(filesRoot, dbRoot)) {
            runCatching {
                com.fauna.ffi.accountStateEraseScope(
                    root.absolutePath,
                    actorIdHex,
                    accountStoreContainerDir(),
                )
            }.onFailure {
                ShellLog.w("AccountStores", "erase of one account under ${root.name} failed: ${it.message}")
            }
        }
    }

    /**
     * One root's share of the sign-out sweep. Hands back what would not go —
     * the shared call no longer throws on a survivor, it reports one, which is
     * the whole reason android can now tell the user instead of logging at them.
     *
     * The `catch` remains for a *hard* failure (a panic crossing the FFI, a
     * root this process cannot even open). That is not a survivor list, so it
     * cannot be reported as one: an empty sweep here would tell the user the
     * device is clean on the one path where we know least about it. It stays a
     * log line, loudly labelled, until it is given a shape of its own.
     */
    private fun eraseRoot(root: File): com.fauna.ffi.FfiEraseSweep {
        return try {
            com.fauna.ffi.accountStateEraseAllScopes(
                root.absolutePath,
                accountStoreContainerDir(),
            )
        } catch (e: Exception) {
            ShellLog.w("AccountStores", "erase under ${root.name} failed outright: ${e.message}")
            com.fauna.ffi.FfiEraseSweep(
                erased = 0u,
                survivors = emptyList(),
                residue = com.fauna.ffi.FfiEraseResidueView(
                    survivors = 0u,
                    credentialsSurvived = false,
                    owesWork = false,
                ),
            )
        }
    }

    /**
     * Where one account's stores live under `root`: `<root>/<actor-id-hex>/`,
     * resolved through the shared Rust derivation so this app, its workers, and
     * every other app agree on the layout.
     */
    fun accountRoot(root: File, actorIdHex: String): File =
        File(com.fauna.ffi.accountStateDir(root.absolutePath, actorIdHex))
}
