package com.fauna.app.ui.viewmodel

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.launch

/**
 * The admin pages' reconnect re-read (`docs/goal/behavior/admin.md` § Errors &
 * edge cases, *Controls wait for their read*): a read that has not answered is
 * read again when the nest connection comes back, with no navigation. Each
 * admin view-model collects [ApiClient.reconnectTick][com.fauna.app.core.ApiClient.reconnectTick]
 * through this one helper, so the rule is written once.
 *
 * [answered] is the page's read state — a shared machine's `readState()` being
 * `RESOLVED`, the NAT machine's `selectPossibility()` being possible, or the
 * page's own read flag. An answered page is NOT read again, so a draft typed
 * over it survives; a re-read that fails after one succeeded keeps the earlier
 * answer (the shared machines keep it). The tui twin is
 * `apps/fauna-tui/src/admin/mod.rs`'s `reconnect_reread_op`.
 *
 * Every entry to a page reads it again by construction: each admin page is a
 * nav destination whose `hiltViewModel()` is scoped to its back-stack entry,
 * so a navigation to it builds a fresh view-model whose `init` reads.
 */
fun CoroutineScope.rereadOnReconnect(
    ticks: Flow<Unit>,
    answered: () -> Boolean,
    reread: () -> Unit,
): Job = launch {
    ticks.collect {
        if (!answered()) reread()
    }
}

/**
 * The admin pages' Back-return re-read (`docs/goal/behavior/admin.md` § Errors &
 * edge cases, *Controls wait for their read*: every entry to a page reads it
 * again). A forward navigation builds a fresh view-model whose `init` reads; a
 * return by Back to a page still on the back stack reuses the old view-model, so
 * the screen's composition entering again calls [onEnter]. The first entry is
 * skipped — `init` has just read — and a trip to the background and back does
 * not re-compose the page, so it never re-reads an answered page (a typed draft
 * survives). The view-model owns one; [AdminEntryRereadEffect] drives it.
 */
class AdminEntryReread(private val reread: () -> Unit) {
    private var entered = false

    fun onEnter() {
        if (entered) reread() else entered = true
    }
}

/** Calls [AdminEntryReread.onEnter] once per composition entry of the page. */
@androidx.compose.runtime.Composable
fun AdminEntryRereadEffect(entry: AdminEntryReread) {
    androidx.compose.runtime.LaunchedEffect(entry) { entry.onEnter() }
}
