package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.conversations.ConversationsManagerHost
import com.fauna.app.core.search.SearchManagerHost
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_search.SearchSnapshot
import uniffi.fauna_conversations.ConversationsManager
import javax.inject.Inject

/**
 * Thin proxy over the shared, stateful Search-page manager
 * ([com.fauna.ffi.FfiSearchManager] via [SearchManagerHost]) — the `FeedVM`/
 * apple-`SearchVM` pattern (docs/goal/ui/search.md § State & data shape). All
 * query/filter/paging/merge decisions live in the shared Rust manager; this
 * VM holds no page limit, no result list, and no client-side filter — it only
 * exposes the shared [snapshot] and routes gestures to the manager.
 */
@HiltViewModel
class SearchVM @Inject constructor(
    private val host: SearchManagerHost,
    private val conversationsHost: ConversationsManagerHost,
) : ViewModel() {

    /** The whole renderable Search page. */
    val snapshot: StateFlow<SearchSnapshot?> get() = host.snapshot

    /**
     * The shared conversations manager, for routing an activated
     * `search-result-item` whose [uniffi.fauna_client_search.SearchNav] target
     * is `Draft`/`Mail` into the conversations page's own gesture — the thread-
     * select/new-thread-compose calls [com.fauna.app.ui.screen.conversations.ConversationListScreen]
     * already makes on a `conversation-item`/`new-conversation-button` click,
     * exactly mirrored here (`docs/goal/ui/search.md` § Where logic lives →
     * *Result navigation (deep link)*; tui's `open_result`,
     * `apps/fauna-tui/src/search.rs`, is the lead-app reference). Mirrors
     * [ConversationsVM.conversationsManager].
     */
    val conversationsManager: ConversationsManager get() = conversationsHost.manager

    /**
     * Enter the page with the query navigated in from the top-bar search
     * field (a plain local buffer there, per § User actions — submit reads it
     * explicitly). Builds/reuses the manager, retries the local-index attach
     * if it hasn't landed yet, and fires the query under the manager's
     * current filter (`TYPE_FILTER_ALL` on a fresh manager).
     */
    fun start(query: String) {
        val m = host.manager() ?: return
        viewModelScope.launch { host.attachLocalIndexIfNeeded() }
        if (query.isBlank()) return
        viewModelScope.launch { m.runQuery(query, m.snapshot().typeFilter) }
    }

    /** `search-type-filter` — re-fire the **last fired** query (not any live
     *  buffer) under a new filter token; a no-op before any search has run. */
    fun setTypeFilter(token: String) {
        val m = host.manager() ?: return
        val lastQuery = m.snapshot().query
        if (lastQuery.isBlank()) return
        viewModelScope.launch { m.runQuery(lastQuery, token) }
    }

    /** `search-load-more-button` — a no-op when nothing has been searched or
     *  the affordance isn't offered (the manager guards both). */
    fun loadMore() {
        val m = host.manager() ?: return
        viewModelScope.launch { m.loadMore() }
    }
}
