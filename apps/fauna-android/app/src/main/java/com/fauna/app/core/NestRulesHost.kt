package com.fauna.app.core

import android.content.Context
import com.fauna.ffi.FfiNestClient
import com.fauna.ffi.FfiNestRulesBook
import com.fauna.ffi.FfiNestRulesRead
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.json.JSONArray
import org.json.JSONObject
import uniffi.fauna_client_policy.NestRulesSummary
import uniffi.fauna_client_policy.StoredNestRules
import java.io.File
import java.util.Base64
import java.util.Collections

/**
 * The member's side of the nest rules on android (`nest-rules.md` § The nest
 * content policy → *Carriage*, § Acceptance at joining, and a change of rules)
 * — a paint of the shared `FfiNestRulesBook` (`libs/fauna-ffi/src/nest_rules.rs`
 * over `fauna_client_policy::NestRulesBook`). tui's `nest_rules.rs` and web's
 * `#lib/nestRules.svelte.ts` are the sibling legs.
 *
 * Everything that decides — verifying the nest's signature, keeping the
 * last-accepted document per nest, saying whether a read was a change, the rule
 * sets and keyword/labeler factors the engine folds, the summary the settings
 * surface paints — is shared Rust behind the book. What is left here is what the
 * design lets diverge: where the book's records live (`filesDir`, install-scoped
 * like the region record — a nest's signed document is public, never a secret,
 * so a change made while the app was closed is still found by its newer
 * sequence); when to read (every transition to `Connected` and every
 * `StaleSurfaces::nest_info` push); and the paint.
 *
 * In every flavor, the kids one included: a ward is bound by its nest's rules.
 */
object NestRulesHost {
    private const val FILE = "nest-rules-book.json"

    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
    private val reading = Mutex()

    @Volatile
    private var book: FfiNestRulesBook? = null

    @Volatile
    private var record: File? = null

    /** The session nest's identity, once its self-description was read — the
     *  nest every item on screen is read through. */
    @Volatile
    private var sessionNestId: String? = null

    private val _revision = MutableStateFlow(0)

    /** Bumped whenever what the book answers may have changed. The composed
     *  verdict's surfaces repaint through [RegionPlaneHost.revision], which a
     *  bump here also moves. */
    val revision: StateFlow<Int> = _revision.asStateFlow()

    private val _changedNotice = MutableStateFlow<String?>(null)

    /** The changed notice's nest domain until the member opens the rules
     *  surface; `null` when there is none. */
    val changedNotice: StateFlow<String?> = _changedNotice.asStateFlow()

    /** Open the book at launch, the persisted records restored (each verified
     *  again) AHEAD of the first read. Idempotent. */
    @Synchronized
    fun open(context: Context) {
        if (book != null) return
        val file = File(context.filesDir, FILE)
        record = file
        book = try {
            FfiNestRulesBook.restore(readRecords(file))
        } catch (e: Exception) {
            // An unreadable record starts the book empty; the first read fills it.
            android.util.Log.w("NestRulesHost", "stored book not restorable: ${e::class.simpleName}: ${e.message}")
            FfiNestRulesBook()
        }
        bump()
    }

    /** The book, opened on first use when [open] has not run (a host test). */
    private fun bookOrOpen(): FfiNestRulesBook =
        book ?: synchronized(this) { book ?: FfiNestRulesBook().also { book = it } }

    /**
     * Read the session nest's self-description past the connection's memo and
     * fold it — at every `Connected` and on every `nest_info` push. Best-effort:
     * a failed read leaves the last-known rules in force and never throws into
     * the connection pump; a change persists the book and raises the notice.
     */
    fun refresh(client: FfiNestClient) {
        scope.launch { read(client) }
    }

    /**
     * The session nest's domain as its self-description named it — the
     * publisher the admin's editor writes under (`FfiRuleEditor.forNest`). Read
     * now, awaited, when no read has landed yet; `null` when it cannot be read.
     */
    suspend fun sessionDomain(client: FfiNestClient): String? {
        sessionDomain?.let { return it }
        read(client)
        return sessionDomain
    }

    @Volatile
    private var sessionDomain: String? = null

    private suspend fun read(client: FfiNestClient) {
        reading.withLock {
            val b = bookOrOpen()
            try {
                val r = b.refresh(client)
                sessionNestId = r.nestId
                sessionDomain = r.domain
                when (val read = r.read) {
                    is FfiNestRulesRead.Changed -> {
                        _changedNotice.value = read.domain
                        persist(b)
                    }
                    is FfiNestRulesRead.First -> persist(b)
                    is FfiNestRulesRead.Refused ->
                        android.util.Log.w("NestRulesHost", "self-description refused, last-known rules stay: ${read.reason}")
                    else -> Unit
                }
            } catch (e: Exception) {
                android.util.Log.w("NestRulesHost", "fauna.nest.info failed: ${e::class.simpleName}: ${e.message}")
            } finally {
                bump()
            }
        }
    }

    /** The departing session's nest goes on an identity change; the book (a
     *  record per nest identity, public) stays for the next sign-in. */
    fun clearSession() {
        sessionNestId = null
        sessionDomain = null
        _changedNotice.value = null
        bump()
    }

    /** The member opened the rules surface — the notice has done its job. */
    fun dismissNotice() {
        _changedNotice.value = null
    }

    /** The book and the session nest it binds, or `null` before both exist —
     *  what [RegionPlaneHost.render] passes to the composed call. */
    fun source(): Pair<FfiNestRulesBook, String>? {
        val b = book ?: return null
        val nest = sessionNestId ?: return null
        return b to nest
    }

    /** What the settings rules surface paints for the session nest; `null`
     *  when it published none (or before its self-description was read). */
    fun summary(): NestRulesSummary? {
        val (b, nest) = source() ?: return null
        return try {
            b.summary(nest, RegionPlaneHost.UI_LANG)
        } catch (e: Exception) {
            null
        }
    }

    private fun bump() {
        _revision.update { it + 1 }
        RegionPlaneHost.invalidate()
    }

    /** Drop the book, the session nest and both witness sets — a host test's
     *  reset. */
    @androidx.annotation.VisibleForTesting
    @Synchronized
    internal fun resetForTest() {
        book = null
        record = null
        sessionNestId = null
        _changedNotice.value = null
        blocked.clear()
        painted.clear()
        bump()
    }

    /** Point the session at [nestId] with [testBook] — a host test's seam. */
    @androidx.annotation.VisibleForTesting
    internal fun installForTest(testBook: FfiNestRulesBook, nestId: String) {
        book = testBook
        sessionNestId = nestId
        bump()
    }

    // ── Persistence: the book's signed records, as JSON under filesDir ───────

    private fun persist(b: FfiNestRulesBook) {
        val file = record ?: return
        try {
            val arr = JSONArray()
            for (r in b.stored()) {
                arr.put(
                    JSONObject()
                        .put("nest_id", r.nestId)
                        .put("domain", r.domain)
                        .put("document", b64(r.document))
                        .put("sequence", r.sequence.toString())
                        .put("issued_at", r.issuedAt)
                        .put("signature", b64(r.signature)),
                )
            }
            val tmp = File(file.parentFile, "${file.name}.tmp")
            tmp.writeText(arr.toString())
            if (!tmp.renameTo(file)) file.writeText(arr.toString())
        } catch (e: Exception) {
            android.util.Log.w("NestRulesHost", "persist failed: ${e::class.simpleName}: ${e.message}")
        }
    }

    private fun readRecords(file: File): List<StoredNestRules> {
        if (!file.exists()) return emptyList()
        val arr = JSONArray(file.readText())
        return (0 until arr.length()).map { i ->
            val o = arr.getJSONObject(i)
            StoredNestRules(
                nestId = o.getString("nest_id"),
                domain = o.getString("domain"),
                document = Base64.getDecoder().decode(o.getString("document")),
                sequence = o.getString("sequence").toULong(),
                issuedAt = o.getLong("issued_at"),
                signature = Base64.getDecoder().decode(o.getString("signature")),
            )
        }
    }

    private fun b64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)

    // ── Convention 17: "a nest Block never renders silent" ───────────────────
    //
    // The region's twin ([RegionPlaneHost.witnessBlocked]): `blocked` holds the
    // on-screen items whose composed verdict a nest rule blocks, registered from
    // the verdict each surface computed; `painted` the nest block placeholders
    // actually composed. tui's and web's `nest_block_render`.
    private val blocked: MutableSet<String> = Collections.synchronizedSet(HashSet())
    private val painted: MutableSet<String> = Collections.synchronizedSet(HashSet())

    /** Register (or clear) an on-screen item's verdict-side walk. */
    fun witnessBlocked(key: String, isBlocked: Boolean) {
        if (isBlocked) blocked.add(key) else blocked.remove(key)
    }

    /** Register (or clear) a composed nest BLOCK placeholder. */
    fun witnessPainted(key: String, isPainted: Boolean) {
        if (isPainted) painted.add(key) else painted.remove(key)
    }

    /** The `nest_block_render` state field
     *  (`tests/e2e-unified/helpers/frame_invariants.py`). */
    fun blockRenderState(): Pair<Int, Int> = blocked.size to painted.size
}
