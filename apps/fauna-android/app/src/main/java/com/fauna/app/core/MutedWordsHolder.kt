package com.fauna.app.core

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_core.MutedKeyword

/**
 * The one in-memory copy of the user's muted-word list every collapse and
 * toast gate in this process reads (`behavior/notifications.md` § Muted words
 * → *Where logic lives*: the list each app already holds for its conversation
 * collapse, never a second read) — the conversation bubbles, the notifications
 * page, and the knock toast, which fires with no screen mounted. The android
 * twin of linux's `conversations::muted_keywords_cache()` and tui's
 * `SettingsState::muted_words`.
 *
 * [ApiClient] owns the instance: it [refresh]es at connect and on every
 * store-change notice (a word muted on another device), and [publish]es the
 * list a Settings write returns (a gesture's own write fires no notice).
 * A failed read keeps the list; an equal answer publishes nothing.
 */
class MutedWordsHolder(
    private val scope: CoroutineScope,
    private val read: suspend () -> List<MutedKeyword>,
) {
    private val _words = MutableStateFlow<List<MutedKeyword>>(emptyList())
    val words: StateFlow<List<MutedKeyword>> = _words.asStateFlow()

    /** Re-read the list from the account store (best-effort). */
    fun refresh() {
        scope.launch { runCatching { read() }.getOrNull()?.let { _words.value = it } }
    }

    /** Publish a list the caller already holds (a Settings write's answer). */
    fun publish(words: List<MutedKeyword>) {
        _words.value = words
    }

    /** The identity ended: an incoming account must not collapse by the
     *  departing one's words. */
    fun clear() {
        _words.value = emptyList()
    }
}
