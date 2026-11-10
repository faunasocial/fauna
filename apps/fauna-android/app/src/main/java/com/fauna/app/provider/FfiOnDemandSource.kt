package com.fauna.app.provider

import android.content.Context
import android.provider.DocumentsContract
import com.fauna.app.core.AccountStores
import com.fauna.app.core.HexUtil
import com.fauna.app.core.SessionAccount
import com.fauna.app.core.ShellLog
import com.fauna.ffi.FfiBearerProvider
import com.fauna.ffi.FfiChangeSignerProvider
import com.fauna.ffi.FfiFileProviderAck
import com.fauna.ffi.FfiFileProviderHost
import com.fauna.ffi.FfiFileProviderItem
import com.fauna.ffi.FfiNestClient
import com.fauna.ffi.FfiOnDemandPrefsStore
import com.fauna.ffi.actorIdFromSecret
import com.fauna.ffi.actorScopedFolderRef
import com.fauna.ffi.folderRefForRow
import com.fauna.ffi.backupKeyDerive
import com.fauna.ffi.capabilityHostPresenceSets
import com.fauna.ffi.hexFull
import com.fauna.ffi.installCapabilityHostCustody
import com.fauna.ffi.machineChangeSigner
import com.fauna.ffi.machineRetainedKeyCustody
import com.fauna.ffi.mintBearer
import com.fauna.ffi.onDemandParkedPresences
import com.fauna.ffi.onDemandPresencePlan
import com.fauna.ffi.syncSetPark
import dagger.hilt.android.qualifiers.ApplicationContext
import uniffi.fauna_devices_machine.FolderSummary
import uniffi.fauna_folders_machine.PresenceSet
import uniffi.fauna_folders_machine.SetPark
import java.io.File
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/**
 * The production [OnDemandSource]: one shared-Rust `FfiFileProviderHost` per
 * desired set, built through the capability constructor `appDeadOwnedTree`
 * (`on-demand-files-android.md` § Android SAF DocumentsProvider binding, *one
 * construction path*) — the owner `BackupKey`, a bearer source and a change
 * signer source, never the identity seed. The OS may start this process for
 * another app's picker with no app session running, so everything here is read
 * from the account's own secure storage ([SessionAccount]) rather than borrowed
 * from `ApiClient`: the provider holds its own nest connection (for the set
 * list) and mints its own bearers.
 *
 * The signer is the machine principal the shared account runtime enrolls at
 * every sign-in (`ApiClient.startAccountRuntime` → the ceremony minting the
 * writer key and its `[RenewBearer, SyncWrite]` grant into the T10 slot, which
 * on android is the `EncryptedSharedPreferences` store lent at launch). The
 * provider runs in the app's process, so its host reads that slot itself
 * (`machineChangeSigner`, shared Rust) at every write — no copy into a second
 * store, no re-provisioning watch, and the writer secret never enters Kotlin
 * memory (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
 * *The capability host* — the android arm). On a machine the ceremony has not
 * enrolled yet a closed write is held in the kept root and uploads at the first
 * sweep after it is; a create, delete, rename or move there is refused to the
 * caller, like any write that cannot be recorded.
 *
 * Teardown rides [AccountStores.registerCloser] — every sign-out and account
 * switch runs it: hosts dropped, the nest connection closed, the account's
 * cache root deleted (recorded bodies the nest can serve again), the kept root
 * left for the owning account's next session, and the provider notified so it
 * re-announces its roots.
 *
 * It is also the Folders page's [OnDemandToggle]: the `folder-on-demand-toggle`
 * reads and writes the same preference store the plan reads, and a flip
 * re-plans and re-announces the roots so the set appears in or leaves Files at
 * once.
 */
@Singleton
class FfiOnDemandSource @Inject constructor(
    @ApplicationContext private val context: Context,
    private val sessionAccount: SessionAccount,
    private val accountStores: AccountStores,
) : OnDemandSource, OnDemandToggle {

    /** The signed-in account's capability inputs, resolved once per account. */
    private class Account(
        val actorHex: String,
        val secret: ByteArray,
        val dialUrl: String,
        val deviceId: ByteArray,
        val backupKey: ByteArray,
        /** The principal-slot reader every host of this account signs through. */
        val signer: FfiChangeSignerProvider,
    ) {
        var nest: FfiNestClient? = null
        var bearer: String? = null
        var bearerExpiresAtMs: Long = 0
    }

    private val lock = Mutex()
    private var account: Account? = null
    /** Desired sets by scoped id → the set's bare `FolderRef` wire string. */
    private var desired: Map<String, Pair<OnDemandSet, String>> = emptyMap()
    /** The sets the last plan presents, by scoped id — the park fold's presences. */
    @Volatile private var presented: Map<String, PresenceSet> = emptyMap()
    private val reconcileTicks = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    private val hosts = java.util.concurrent.ConcurrentHashMap<String, OnDemandSetHost>()
    private val teardownListeners = java.util.concurrent.CopyOnWriteArrayList<() -> Unit>()

    init {
        accountStores.registerCloser("on-demand-provider") { teardown() }
    }

    override fun activeActorHex(): String? = currentAccount()?.actorHex

    override fun liveHosts(): Map<String, OnDemandSetHost> = HashMap(hosts)

    override val reconciled: Flow<Unit> get() = reconcileTicks

    override fun onTeardown(listener: () -> Unit) {
        teardownListeners.add(listener)
    }

    override suspend fun sets(refresh: Boolean): List<OnDemandSet> = lock.withLock {
        val acct = currentAccount() ?: return@withLock emptyList()
        if (refresh || desired.isEmpty()) {
            runCatching { replan(acct) }
                .onFailure { ShellLog.w(TAG, "set list unavailable — keeping the last answer: ${it.message}") }
        }
        desired.values.map { it.first }
    }

    override suspend fun host(scopedId: String): OnDemandSetHost? {
        hosts[scopedId]?.let { return it }
        return lock.withLock {
            hosts[scopedId]?.let { return@withLock it }
            val acct = currentAccount() ?: return@withLock null
            if (desired.isEmpty()) runCatching { replan(acct) }
            val folderId = desired[scopedId]?.second ?: return@withLock null
            val ffi = FfiFileProviderHost.appDeadOwnedTree(
                acct.dialUrl,
                HexUtil.hexToBytes(acct.actorHex),
                acct.deviceId,
                DEVICE_LABEL,
                acct.backupKey,
                bearerProvider(acct),
                acct.signer,
                accountStores.syncStateDir(),
                folderId,
                context.filesDir.absolutePath,
                context.cacheDir.absolutePath,
                accountStores.predecessorChain(acct.actorHex),
            )
            FfiSetHost(ffi).also { hosts[scopedId] = it }
        }
    }

    override suspend fun identify(folder: FolderSummary): OnDemandIdentity? = withContext(Dispatchers.IO) {
        val actorHex = currentAccount()?.actorHex ?: return@withContext null
        runCatching {
            folderRefForRow(folder.id, folder.mlsGroupId, folder.homeNestUrl)?.let { ref ->
                actorScopedFolderRef(actorHex, ref)?.let {
                    // Fail-safe like `binding_section`: an absent access reads reader.
                    OnDemandIdentity(it.scopedId, ref, folder.role == "member" && folder.access != "writer")
                }
            }
        }.getOrNull()
    }

    override suspend fun read(sets: List<OnDemandIdentity>): Map<String, OnDemandToggleState> =
        withContext(Dispatchers.IO) {
            val prefs = prefsStore()
            val stronger = strongerRefs().toSet()
            // The fold reads the plan's presences: a process that has served no
            // picker yet runs its first reconcile here (a failure keeps none).
            if (presented.isEmpty() && sets.isNotEmpty()) runCatching { sets(refresh = false) }
            val parked = parkedScopedIds(sets)
            sets.associate { set ->
                set.scopedId to OnDemandToggleState(
                    shown = prefs.isEnabled(set.scopedId),
                    stronger = set.folderRef in stronger,
                    parked = set.scopedId in parked,
                )
            }
        }

    /**
     * The scoped ids among [sets] whose row is warned — the shared fold
     * (`on_demand_parked_presences`) over each presented set's park read
     * (`sync_set_park`, the host's own state DB, read in-process), the row's
     * own `reader` reading taken over the last plan's (`file-sync.md`
     * § Multi-writer shared sets → *Revocation on a capability host*). A set
     * this device does not present has no park to read. A read that fails
     * shows the set parked: the warning fails closed.
     */
    private fun parkedScopedIds(sets: List<OnDemandIdentity>): Set<String> {
        val plan = presented
        val presences = sets.mapNotNull { id ->
            plan[id.scopedId]?.let { p -> id.scopedId to p.copy(readOnly = p.readOnly || id.rowReadsReader) }
        }
        if (presences.isEmpty()) return emptySet()
        val stateDir = accountStores.syncStateDir()
        val reads = presences.map { (_, p) -> parkRead(stateDir, p.folderId) }
        val parkedFolders = runCatching { onDemandParkedPresences(presences.map { it.second }, reads) }
            .getOrDefault(emptyList()).toSet()
        return presences.filter { it.second.folderId in parkedFolders }.map { it.first }.toSet()
    }

    /** One set's park, read in-process off the host's state DB; a failed read reads parked. */
    private fun parkRead(stateDir: String, folderId: String): SetPark =
        runCatching { syncSetPark(stateDir, folderId) }
            .onFailure { ShellLog.w(TAG, "park read failed — showing the set parked: ${it.message}") }
            .getOrDefault(SetPark(folderId, true, 0u))

    /**
     * Every set the last plan presents, by display name, with its park read —
     * the observation seam the e2e bridge reads for the host's mark (the
     * revocation witness); the page reads [read]'s fold instead.
     */
    suspend fun presentedParks(): List<Pair<String, SetPark>> = withContext(Dispatchers.IO) {
        if (presented.isEmpty()) runCatching { sets(refresh = false) }
        val stateDir = accountStores.syncStateDir()
        presented.values.map { it.name to parkRead(stateDir, it.folderId) }
    }

    override suspend fun setShown(scopedId: String, shown: Boolean) = withContext(Dispatchers.IO) {
        prefsStore().setEnabled(scopedId, shown)
    }

    override suspend fun replan() {
        withContext(Dispatchers.IO) { sets(refresh = true) }
        val authority = "${context.packageName}.documents"
        context.contentResolver.notifyChange(DocumentsContract.buildRootsUri(authority), null)
        context.contentResolver.notifyChange(
            DocumentsContract.buildChildDocumentsUri(authority, FaunaDocumentsProvider.ROOT_DOCUMENT_ID),
            null,
        )
    }

    /**
     * The device-local toggle store the plan and the page share — the active
     * account's, carried from a predecessor scope first after an identity
     * succession ([carriedOnDemandPrefs]).
     */
    private fun prefsStore(): FfiOnDemandPrefsStore {
        val actorHex = accountStores.activeActorHex()
            ?: return FfiOnDemandPrefsStore(File(accountStores.syncStateDir(), ON_DEMAND_PREFS_FILE).absolutePath)
        return carriedOnDemandPrefs(actorHex, accountStores.predecessorsOf(actorHex)) {
            File(accountStores.syncStateDirFor(it), ON_DEMAND_PREFS_FILE)
        }
    }

    /** The bare refs a watched-directory ingress feeds — a stronger presence the plan leaves out. */
    private suspend fun strongerRefs(): List<String> = runCatching {
        accountStores.database().watchedDirectoryDao().getEnabled().first().map { it.folderId }
    }.getOrDefault(emptyList())

    /**
     * One presence reconcile: the account's sets — its own and the ones
     * shared with it — through the shared plan (`on_demand_presence_plan`),
     * minus the sets this device feeds through an ingress — the explicit act
     * outranks the ambient default, so no set has two engines writing it from
     * one device id. A removal drops its host. Throws when the set list could
     * not be read: the caller keeps the last answer.
     */
    private suspend fun replan(acct: Account) {
        val nest = acct.nest ?: FfiNestClient(acct.dialUrl, acct.secret).also {
            it.connect()
            acct.nest = it
        }
        // Every set the account holds, mapped in shared Rust
        // (`capability_host_presence_sets`; `on-demand-files-shared-sets.md` § Shared sets
        // on a capability host, decision 3): an own folder is a delivery seat
        // where THIS device's place accepts, a folder shared with the account
        // once its share was accepted, read-only unless a writer; a group set
        // from the app's ring mirror in the hosts' state dir (`group-set.md`
        // § decision 12). It reads the custody the hosts read, with the hosts'
        // own capability. A read that fails throws: skip this reconcile
        // rather than tearing down presences on a state we could not see.
        val sets = capabilityHostPresenceSets(
            nest.folders(),
            acct.dialUrl,
            HexUtil.hexToBytes(acct.actorHex),
            acct.deviceId,
            acct.backupKey,
            bearerProvider(acct),
            acct.signer,
            accountStores.syncStateDir(),
        )
        val plan = onDemandPresencePlan(acct.actorHex, sets, strongerRefs(), prefsStore(), hosts.keys.toList(), null)
        for (removal in plan.remove) hosts.remove(removal.identifier)?.close()
        desired = (plan.add + plan.keep).associate { p ->
            p.scopedId to (OnDemandSet(p.scopedId, p.set.name, p.set.readOnly) to p.set.folderId)
        }
        presented = (plan.add + plan.keep).associate { it.scopedId to it.set }
        reconcileTicks.tryEmit(Unit)
    }

    /**
     * The bearer the host presents: this account's, re-minted a minute before
     * expiry. Empty when none can be minted — the host fails closed.
     */
    private fun bearerProvider(acct: Account) = object : FfiBearerProvider {
        override fun currentBearer(): String = synchronized(acct) {
            val cached = acct.bearer
            if (cached != null && System.currentTimeMillis() < acct.bearerExpiresAtMs) return cached
            runCatching { runBlocking { mintBearer(acct.dialUrl, acct.secret) } }
                .onSuccess {
                    acct.bearer = it.token
                    acct.bearerExpiresAtMs = (it.expiresAt.toLong() - 60) * 1000L
                }
                .onFailure { ShellLog.w(TAG, "bearer mint failed: ${it.message}") }
                .getOrNull()?.token.orEmpty()
        }
    }

    /**
     * The session account as secure storage holds it now, or null when signed
     * out. An account other than the one cached tears the old one down first.
     */
    private fun currentAccount(): Account? {
        val secretHex = sessionAccount.secretHex ?: return null.also { teardownIfHeld() }
        val secret = HexUtil.hexToBytes(secretHex)
        val actorHex = runCatching { hexFull(actorIdFromSecret(secret)) }.getOrNull() ?: return null
        synchronized(this) {
            account?.let { if (it.actorHex == actorHex) return it }
        }
        teardownIfHeld()
        val nestUrl = sessionAccount.nestUrl ?: return null
        val deviceId = sessionAccount.resolveDeviceId()?.let { HexUtil.hexToBytes(it) } ?: return null
        val backupKey = runCatching { backupKeyDerive(secret) }.getOrNull() ?: return null
        val dialUrl = uniffi.fauna_launch_machine.resolvedDialUrl(nestUrl)
        val signer = runCatching { machineChangeSigner(HexUtil.hexToBytes(actorHex)) }.getOrNull() ?: return null
        // The hosts read the account's folder-key custody as this device
        // (`on-demand-files-shared-sets.md` § Shared sets on a capability host, decision
        // 1′): their replica unwraps with the machine principal's key and the
        // slot's retained-key custody, installed here before any host builds.
        runCatching {
            val actor = HexUtil.hexToBytes(actorHex)
            installCapabilityHostCustody(
                actor,
                machineRetainedKeyCustody(actor, accountStores.accountStoreContainerDir()),
            )
        }.onFailure { ShellLog.w(TAG, "retained-key custody not installed: ${it.message}") }
        return synchronized(this) {
            account ?: Account(actorHex, secret, dialUrl, deviceId, backupKey, signer).also { account = it }
        }
    }

    private fun teardownIfHeld() {
        if (synchronized(this) { account != null }) teardown()
    }

    /** Sign-out / switch: hosts dropped, cache root deleted, kept root left, roots re-announced. */
    private fun teardown() {
        val gone = synchronized(this) { account.also { account = null } }
        desired = emptyMap()
        presented = emptyMap()
        val held = HashMap(hosts)
        hosts.clear()
        held.values.forEach { runCatching { it.close() } }
        if (gone != null) {
            gone.nest?.let { nest -> runCatching { runBlocking { nest.disconnect() } }; nest.close() }
            File(context.cacheDir, "on-demand/${gone.actorHex}").deleteRecursively()
        }
        teardownListeners.forEach { runCatching { it() } }
    }

    private class FfiSetHost(private val ffi: FfiFileProviderHost) : OnDemandSetHost {
        override suspend fun enumerate(parentRel: String) = ffi.enumerate(parentRel).map { it.toItem() }
        override suspend fun item(rel: String) = ffi.item(rel)?.toItem()
        override suspend fun openForRead(rel: String) = ffi.openForRead(rel)
        override suspend fun openForWrite(rel: String) =
            ffi.openForWrite(rel).let { OnDemandWriteOpen(it.path, it.baseContentVersion) }
        override suspend fun closedWrite(rel: String, baseVersion: ByteArray) =
            ffi.closedWrite(rel, baseVersion).toAck()
        override suspend fun createDocument(rel: String) = ffi.createDocument(rel).toAck()
        override suspend fun deleteDocument(rel: String) = ffi.deleteDocument(rel)
        override suspend fun renameDocument(fromRel: String, toRel: String) =
            ffi.renameDocument(fromRel, toRel).toAck()
        override suspend fun refresh() = ffi.refresh()
        override suspend fun sweepKeptRoot() =
            ffi.sweepKeptRoot().let { OnDemandSweep(it.recorded, it.pending, it.evicted) }
        override fun close() = ffi.close()

        private fun FfiFileProviderItem.toItem() =
            OnDemandItem(rel, name, sizeBytes, mtime, isDir)

        private fun FfiFileProviderAck.toAck() = OnDemandAck(acked, contentChanged, excluded)
    }

    companion object {
        private const val TAG = "FfiOnDemandSource"

        /**
         * Android's **app default** (`devices.md` § Device names, class (2)):
         * the label every register leg of this app passes — the engine hosts
         * and the account runtime's enrollment ceremony alike.
         */
        const val DEVICE_LABEL = "fauna-android"
    }
}
