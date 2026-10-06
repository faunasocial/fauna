package com.fauna.app.core.search

import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiSearchManager
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.fauna_client_search.SearchSnapshot
import uniffi.fauna_client_search.SearchSnapshotObserver
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Process-wide owner of the observer→snapshot bridge for the shared Search-
 * page [FfiSearchManager] — mirrors [com.fauna.app.core.feed.FeedManagerHost].
 * The manager itself is a connection-bound singleton owned by [ApiClient]
 * (built lazily over the post-auth WS-RPC socket, torn down on sign-out);
 * this host owns only the [SearchSnapshotObserver] and the [snapshot]
 * `StateFlow`, plus the local-index attach retry (docs/goal/ui/search.md
 * § Implementation status today).
 */
@Singleton
class SearchManagerHost @Inject constructor(
    private val api: ApiClient,
) {
    private val _snapshot = MutableStateFlow<SearchSnapshot?>(null)

    /** Compose screens `collectAsState()` this; every manager notification
     *  pushes a fresh snapshot. Null before the manager is built (nest not
     *  yet connected) and after sign-out. */
    val snapshot: StateFlow<SearchSnapshot?> = _snapshot.asStateFlow()

    /** The manager whose `snapshot()` [observer] republishes — held so the
     *  arbitrary-Rust-thread `onChanged` callback reads the *current* manager
     *  even across a sign-out→sign-in rebuild. */
    private var seen: FfiSearchManager? = null

    /** Whether this login's local sealed-index arm has attached — set only on
     *  a `true` [ApiClient.attachLocalSearchIndex] return, so a `false`
     *  attempt (no session yet, or this actor has no mail) can retry on the
     *  next page entry rather than being treated as permanently settled
     *  (mirrors apple's `SearchVM.indexAttached`). */
    private var indexAttached = false

    private val observer = object : SearchSnapshotObserver {
        override fun onChanged() {
            _snapshot.value = seen?.snapshot()
        }
    }

    /** The shared Search manager over the current connection, or null until
     *  the nest is connected. Reseeds [snapshot] and resets the attach-retry
     *  flag when [ApiClient] hands back a freshly-built manager (the sign-out
     *  →sign-in case — the new instance has this host's observer
     *  re-registered). */
    fun manager(): FfiSearchManager? {
        val m = api.searchManager(observer) ?: return null
        if (m !== seen) {
            seen = m
            indexAttached = false
            _snapshot.value = m.snapshot()
        }
        return m
    }

    /**
     * Register this login's local sealed-index arm if it hasn't attached yet.
     * A `false` [ApiClient.attachLocalSearchIndex] return is a normal state
     * (no conversations session yet, or this actor has no mail), so
     * [indexAttached] stays false and a later call — the next page entry —
     * can retry once the precondition arrives.
     */
    suspend fun attachLocalIndexIfNeeded() {
        val m = seen ?: return
        if (indexAttached) return
        if (api.attachLocalSearchIndex(m)) indexAttached = true
    }
}
