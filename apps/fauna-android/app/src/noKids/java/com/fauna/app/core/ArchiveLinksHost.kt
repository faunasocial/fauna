package com.fauna.app.core

import android.content.Context
import com.fauna.app.ui.util.resolveComposed
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import uniffi.fauna_archive_import_machine.ArchiveLinkState
import uniffi.fauna_archive_import_machine.ArchiveLinkView
import uniffi.fauna_archive_import_machine.ArchiveLinks
import uniffi.fauna_archive_import_machine.archiveLinkPrompt
import uniffi.fauna_archive_import_machine.archiveLinkedLabel
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Phase two of archive import on android — the account's archive links
 * (`docs/goal/behavior/archive-import.md` § Linking and merging, ruling 8).
 *
 * Everything that decides is shared Rust: the links are the machine crate's
 * `ArchiveLinks`, which the FFI factory builds with the conversations session
 * and registers as its archive-signal sink (`FfiNestClient.archiveLinks()`);
 * each contact's state is its `ArchiveLinkView` and the row's words the crate's
 * `text` composers. This host only drives and caches — the tui `archive_glue` /
 * `archive_links_tick` shape, the FaunaKit `ArchiveLinksStore` twin:
 *
 * - [start] — from [ApiClient]'s connect path once the conversations session is
 *   built: one `driveLinks()` now, then a one-second tick that drives again
 *   whenever `linkWorkDue()` reads true (the sink never sends — it runs inside
 *   the receive loop) and re-reads the views whenever `linkCaptures()` moved;
 * - [refresh] — the Contacts page re-reads the views with its roster;
 * - [answer] / [unlink] — the row's `contact-archive-link-yes` / `-no` /
 *   `contact-archive-unlink`.
 *
 * Stopped by [ApiClient.clearAuth] (through `detachKidsExcised`), so an account
 * switch never paints the outgoing account's links. Compiled out of the kids
 * build with the rest of archive import.
 */
internal class ArchiveLinksHost(private val scope: CoroutineScope) {

    private val _painter = MutableStateFlow(ArchiveLinkPainter.NONE)

    /** The latest read of the views, as the row painter; [ArchiveLinkPainter.NONE]
     *  before the first read and for a login that wired no links. */
    val painter: StateFlow<ArchiveLinkPainter> = _painter

    @Volatile
    private var links: ArchiveLinks? = null

    /** The `linkCaptures()` count the views were last re-read at. */
    @Volatile
    private var seenCaptures: ULong = 0UL

    /** Single-flight guard: one `driveLinks()` at a time (tui's
     *  `archive_links_drive` flag). */
    private val driving = AtomicBoolean(false)

    private var tick: Job? = null

    /** The links the session built, for the archive-import machine's
     *  `attachLinks` before a run; `null` while none are wired. */
    fun current(): ArchiveLinks? = links

    /** Adopt [fresh] and start driving it. Idempotent for the same links. */
    fun start(fresh: ArchiveLinks) {
        synchronized(this) {
            if (links === fresh) return
            stopLocked()
            links = fresh
            seenCaptures = fresh.linkCaptures()
            tick = scope.launch {
                while (isActive) {
                    delay(TICK_MS)
                    onTick(fresh)
                }
            }
        }
        drive(fresh)
        scope.launch { refresh() }
    }

    /** Stop the tick and drop the links and their views. */
    fun stop() = synchronized(this) { stopLocked() }

    private fun stopLocked() {
        tick?.cancel()
        tick = null
        links = null
        seenCaptures = 0UL
        driving.set(false)
        _painter.value = ArchiveLinkPainter.NONE
    }

    /** Re-read every contact's link view. Best-effort: a failed read keeps the
     *  previous views rather than blanking a live prompt. */
    suspend fun refresh() {
        val l = links ?: return
        val fresh = try {
            l.linkViews()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            ShellLog.w("ArchiveLinks", "views: ${e.message}")
            return
        }
        // A stop or a re-point while the read was in flight owns the painter now.
        if (links === l) _painter.value = viewsPainter(fresh)
    }

    /** The row's same-person answer — once, either way. */
    suspend fun answer(contact: String, platform: String, yes: Boolean) {
        val l = links ?: return
        try {
            l.answerLinkToken(contact, platform, yes)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            ShellLog.w("ArchiveLinks", "answer: ${e.message}")
        }
        refresh()
    }

    /** Unlink a linked contact: future merges stop, claimed content stays. */
    suspend fun unlink(contact: String, platform: String) {
        val l = links ?: return
        try {
            l.unlinkToken(contact, platform)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            ShellLog.w("ArchiveLinks", "unlink: ${e.message}")
        }
        refresh()
    }

    private fun onTick(l: ArchiveLinks) {
        if (links !== l) return
        if (l.linkWorkDue()) drive(l)
        val captures = l.linkCaptures()
        if (captures != seenCaptures) {
            seenCaptures = captures
            scope.launch { refresh() }
        }
    }

    private fun drive(l: ArchiveLinks) {
        if (!driving.compareAndSet(false, true)) return
        scope.launch {
            try {
                l.driveLinks()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                ShellLog.w("ArchiveLinks", "drive: ${e.message}")
            } finally {
                if (links === l) driving.set(false)
            }
            refresh()
        }
    }

    companion object {
        /** The tick's cadence — tui's `archive_links_tick`. */
        const val TICK_MS = 1000L
    }
}

/**
 * The row painter over one read of [views]: the views filtered to the row's
 * contact (hex, case-blind), worded by the machine crate's composers. The
 * FaunaKit `archiveLinkRowPaints` twin.
 */
internal fun viewsPainter(views: List<ArchiveLinkView>): ArchiveLinkPainter =
    ArchiveLinkPainter { context, peerId, contactName -> archiveLinkRowPaints(context, views, peerId, contactName) }

internal fun archiveLinkRowPaints(
    context: Context,
    views: List<ArchiveLinkView>,
    peerId: String,
    contactName: String,
): List<ArchiveLinkRowPaint> =
    views
        .filter { it.contact.equals(peerId, ignoreCase = true) }
        .mapNotNull { view ->
            when (view.state) {
                ArchiveLinkState.ASKED -> ArchiveLinkRowPaint.Asked(
                    prompt = resolveComposed(context, archiveLinkPrompt(view, contactName)),
                    contact = view.contact,
                    platform = view.platform,
                )
                ArchiveLinkState.LINKED -> ArchiveLinkRowPaint.Linked(
                    label = resolveComposed(context, archiveLinkedLabel(view)),
                    contact = view.contact,
                    platform = view.platform,
                )
                ArchiveLinkState.OFFERED, ArchiveLinkState.DECLINED, ArchiveLinkState.UNLINKED -> null
            }
        }
