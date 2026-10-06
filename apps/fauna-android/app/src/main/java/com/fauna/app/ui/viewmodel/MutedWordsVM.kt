package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import uniffi.fauna_client_config.MutedWordsSnapshot
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * Drives the **Muted words** Settings sub-page ([MutedWordsScreen]) — the user's
 * personal, sealed, client-held muted-keyword list (moderation.md § Muted keywords /
 * content-moderation-and-ranking.md § Q3). CRUD rides the shared account-store seam
 * via the FFI `muted_keywords_{list,set}` (through [ApiClient]); the whole list is a
 * single user-global list (add/remove = a whole-list set, normalized shared-side).
 * Mirrors linux `settings/muted_words.rs` + web's page; no per-app logic (priority #2).
 * The list is sealed client-side (no WS-RPC kind) and is a *hide/collapse* control —
 * never a spam-queue flag.
 */
@HiltViewModel
class MutedWordsVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    /**
     * The shared page record — the normalized entries, term and weight
     * (newest-set-wins) **and** whether a read has returned. Held whole rather
     * than as a bare list: an empty `keywords` means "no terms" only once
     * `loaded` is true, which is what
     * separates the genuine empty state from a page that has not read yet
     * (`docs/goal/ui/README.md` § *List pages: loading is not empty*). The
     * initial value is the unread state, and a failed load replaces nothing.
     */
    val page = MutableStateFlow(MutedWordsSnapshot(keywords = emptyList(), loaded = false))
    val isLoading = MutableStateFlow(false)
    val errorMessage = MutableStateFlow<String?>(null)

    init {
        load()
        // The store-change notice: another device's word, applied by this
        // app's own pump, appears on the open page with no re-visit. The draft
        // is the screen's own state, and an unchanged record conflates.
        viewModelScope.launch { api.storeChangedTick.collect { load() } }
    }

    /** Load the list. Kept: `mutedKeywordsList` composes a config-store
     *  fetch with a local unseal, not a single NestClient RPC (transport.md
     *  § Request lifecycle step 3's note). */
    fun load() {
        viewModelScope.launch {
            isLoading.value = true
            repeat(HYDRATE_ATTEMPTS) {
                try {
                    page.value = api.mutedKeywordsList()
                    isLoading.value = false
                    return@launch
                } catch (_: Exception) {
                    delay(HYDRATE_RETRY_MS)
                }
            }
            isLoading.value = false
        }
    }

    /** Add a term as a DELTA against the stored list, never the page's copy
     *  wholesale: the shared seam re-reads the list inside its
     *  own CAS update, so a term another device stored since this page loaded
     *  survives this click. Normalization is the seam's (trim / drop blanks /
     *  case-insensitive dedupe); the stored list returns. No-op on blank. */
    fun add(term: String) {
        val t = term.trim()
        if (t.isEmpty()) return
        viewModelScope.launch {
            try {
                page.value = api.mutedKeywordsAdd(t)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    /** Remove a term — add's inverse on the same delta seam; removing a term
     *  already gone is a success no-op (convergence, not an error). */
    fun remove(term: String) {
        viewModelScope.launch {
            try {
                page.value = api.mutedKeywordsRemove(term)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    companion object {
        private const val HYDRATE_ATTEMPTS = 10
        private const val HYDRATE_RETRY_MS = 500L
    }
}
