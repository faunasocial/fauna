package com.fauna.app.provider

/**
 * What [FaunaDocumentsProvider] serves from — the read path of the android
 * on-demand binding (`on-demand-files.md` § Android SAF DocumentsProvider
 * binding). The provider only translates `DocumentsContract` calls into these
 * and builds cursors; it holds no sync state and makes no sync decision. The
 * production source is [FfiOnDemandSource] (one shared-Rust
 * `FfiFileProviderHost` per set); tests install a fake through
 * [FaunaDocumentsProvider.sourceOverride].
 */
interface OnDemandSource {
    /** The active account's actor id (hex), or null when signed out — no root is served then. */
    fun activeActorHex(): String?

    /**
     * The account's desired sets (the shared presence plan's add ∪ keep),
     * re-read from the nest when [refresh] or nothing is cached yet; a failed
     * read keeps the last answer.
     */
    suspend fun sets(refresh: Boolean): List<OnDemandSet>

    /** The set's host, built lazily by the first call that names it; null for a set not desired. */
    suspend fun host(scopedId: String): OnDemandSetHost?

    /** The hosts built so far (the refresh tick walks these). */
    fun liveHosts(): Map<String, OnDemandSetHost>

    /** Called after a sign-out or account switch tore the hosts down, so the provider can notify its roots. */
    fun onTeardown(listener: () -> Unit)
}

/** One set presented as a child of the account's root. */
data class OnDemandSet(
    /** `<ref-component>@<actor-id-hex>` (`local%3A1@…`) — the set's identity, and its document id. */
    val scopedId: String,
    /** The set's name: a display label only, never an identity. A shared set's carries its owner. */
    val name: String,
    /**
     * The account holds the set as a reader (a folder shared with it without a
     * writer grant): nothing in it advertises write, create, delete, rename or
     * move, and the host refuses every write.
     */
    val readOnly: Boolean = false,
)

/** One enumerated row of a set (a file or a synthesized directory). */
data class OnDemandItem(
    /** Folder-relative, forward-slash path. */
    val rel: String,
    val name: String,
    val sizeBytes: Long,
    /** Unix seconds. */
    val mtime: Long,
    val isDir: Boolean,
)

/** A document opened for write: its body in the kept root, and the version the open saw. */
class OnDemandWriteOpen(val path: String, val baseVersion: ByteArray)

/** A write's answer from the host. */
data class OnDemandAck(
    /** The nest recorded the change. Not acked = the body stays in the kept root for the next sweep. */
    val acked: Boolean,
    /** A conflict moved the row to another head: the next open fetches the winner. */
    val contentChanged: Boolean,
    /** The name is ignored (a dotfile, a built-in ignore, a `.faunaignore` pattern): nothing was ingested. */
    val excluded: Boolean,
)

/** What one kept-root sweep did. */
data class OnDemandSweep(
    val recorded: List<String>,
    val pending: List<String>,
    val evicted: List<String>,
)

/** One set's host — `FfiFileProviderHost` built by `appDeadOwnedTree`. */
interface OnDemandSetHost {
    suspend fun enumerate(parentRel: String): List<OnDemandItem>
    suspend fun item(rel: String): OnDemandItem?

    /** The body's path, hydrated into the cache root first; throws when it cannot be fetched. */
    suspend fun openForRead(rel: String): String

    /** The body promoted into the kept root. Every open is matched by exactly one [closedWrite]. */
    suspend fun openForWrite(rel: String): OnDemandWriteOpen

    /** The written descriptor closed: ingest against the open's base; demote on a recorded ack. */
    suspend fun closedWrite(rel: String, baseVersion: ByteArray): OnDemandAck

    /** An empty document at [rel], ingested; an existing or ignored name throws. */
    suspend fun createDocument(rel: String): OnDemandAck

    /** Record-first delete (a directory file by file); throws when it cannot be recorded. */
    suspend fun deleteDocument(rel: String)

    /** Record-first rename or move (a directory file by file); throws when it cannot be recorded. */
    suspend fun renameDocument(fromRel: String, toRel: String): OnDemandAck

    /** Re-pull the set from the nest; true when something changed. */
    suspend fun refresh(): Boolean

    /**
     * Re-drive the start sweep: every kept-root body (a write that did not
     * record — offline, or a process killed before its upload) is ingested,
     * and evictions are observed.
     */
    suspend fun sweepKeptRoot(): OnDemandSweep

    fun close()
}
