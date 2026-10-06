package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.ui.util.getStringFmt
import com.fauna.app.ui.util.resolveLocalized
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_conversations.CrossGroupEviction
import uniffi.fauna_conversations.UnreachableSeatClass
import uniffi.fauna_core.MemberReviewRowText
import javax.inject.Inject

/**
 * One still-open review item: the raw person id (verdict actions need it) plus
 * the shared row-text parts ([ApiClient.memberReviewRowText] —
 * `fauna_core::data::review_row_text`, consumed, never re-derived). `text.who`
 * is resolved from whatever handle [ApiClient.memberReviewHandleForPerson]
 * found at LOAD time — re-resolving it after a Remove would find no seat left
 * to read one off.
 */
data class MemberReviewRow(
    val person: ByteArray,
    val text: MemberReviewRowText,
)

/**
 * Drives the permanent **Members To Review** Settings sub-page
 * ([com.fauna.app.ui.screen.settings.MemberReviewScreen]) — item (iv) of
 * `succession-aftermath.md` § Propagation's two-surface ruling. Renders whatever a review sweep left unanswered; unlike the
 * (not-yet-built-on-android) ephemeral kit-side pass this page has **no sweep
 * gate** — it is reachable, and ordinarily empty, at all times
 * (`docs/goal/ui/settings.md` § Navigation model). Mirrors tui
 * `settings/member_review.rs::review_rows` / linux's port of it; zero shared
 * logic owed here (priority #2) — every seam is
 * `libs/fauna-ffi/src/member_review.rs`.
 *
 * **The verdict is DERIVED, never chosen.** [ApiClient.memberReviewRemove]
 * always evicts first and persists only what the eviction earned; this VM
 * never constructs a verdict of its own.
 */
@HiltViewModel
class MemberReviewVM @Inject constructor(
    private val api: ApiClient,
    @ApplicationContext private val context: Context,
) : ViewModel() {

    /** The open roster, resolved for display, and whether a read has
     *  returned — an empty list means "nothing open" only once [loaded] is
     *  true (`docs/goal/ui/README.md` § *List pages: loading is not empty*). */
    val rows = MutableStateFlow<List<MemberReviewRow>>(emptyList())
    val loaded = MutableStateFlow(false)
    val errorMessage = MutableStateFlow<String?>(null)

    init {
        load()
    }

    /** Re-read the roster. A verdict another device recorded must not be
     *  re-asked here, which is why this always re-reads rather than patching
     *  the cached list in place. Kept: `memberReviewList` composes a
     *  config-store fetch with a local unseal, not a single NestClient RPC
     *  (transport.md § Request lifecycle step 3's note); the per-row handle
     *  lookup is a local, synchronous conversations-cache read. */
    fun load() {
        viewModelScope.launch {
            repeat(HYDRATE_ATTEMPTS) { attempt ->
                try {
                    val reviews = api.memberReviewList()
                    rows.value = reviews.map { review ->
                        val handle = api.memberReviewHandleForPerson(review.person)
                        MemberReviewRow(
                            person = review.person,
                            text = api.memberReviewRowText(review.person, review.reasons, handle),
                        )
                    }
                    loaded.value = true
                    return@launch
                } catch (e: Exception) {
                    if (attempt == HYDRATE_ATTEMPTS - 1) {
                        errorMessage.value = e.message
                    } else {
                        delay(HYDRATE_RETRY_MS)
                    }
                }
            }
        }
    }

    /** Record **Keep** — closes every open item for `person` with no group
     *  changes; a concurrent device may have already answered, which is a
     *  success no-op, never an error. Re-reads after (the answered row drops
     *  out). */
    fun keep(person: ByteArray) {
        viewModelScope.launch {
            try {
                api.memberReviewKeep(person)
                errorMessage.value = null
                load()
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    /** Record **Remove** — evicts `person` from every group of the owner's
     *  they are in NOW (re-derived, never from the stored item; the FFI seam
     *  persists only what the eviction earned, so this app cannot record
     *  `Removed` from its own reasoning). Composes this app's own outcome
     *  message from the returned `evicted`/`failed`/`unreachable` counts
     *  (mirrors linux `member_review.rs::remove_result_message`), then
     *  re-reads the roster regardless: a full eviction drops the row, a
     *  partial one re-renders from the unchanged state. `who` is resolved
     *  from the cached row BEFORE the call — there is no seat left to read a
     *  handle off afterward. */
    fun remove(person: ByteArray) {
        val who = resolveLocalized(context, rows.value.firstOrNull { it.person.contentEquals(person) }?.text?.who)
            ?: context.getString(R.string.settings_recovery_kit_review_unknown_person)
        viewModelScope.launch {
            try {
                val eviction = api.memberReviewRemove(person)
                errorMessage.value = removeResultMessage(context, eviction, who)
                load()
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

/** [CrossGroupEviction] → this app's `error-message` text — `null` when the
 *  row is expected to drop out cleanly on the next re-read. The FFI seam only
 *  invokes the verdict-persisting write once an eviction is COMPLETE
 *  (`failed`/`unreachable` both empty), so a partial eviction here never
 *  means a failed persist — only a seat still standing. A free function
 *  (not a VM member) so it is directly testable, mirroring linux
 *  `member_review.rs::remove_result_message`. */
internal fun removeResultMessage(context: Context, eviction: CrossGroupEviction, who: String): String? {
    if (eviction.failed.isEmpty() && eviction.unreachable.isEmpty()) return null
    val parts = mutableListOf<String>()
    if (eviction.failed.isNotEmpty()) {
        val groups = eviction.evicted.size + eviction.failed.size
        parts += context.getStringFmt(
            R.string.settings_recovery_kit_review_remove_partial,
            who, eviction.evicted.size, groups,
        )
    } else if (eviction.evicted.isNotEmpty()) {
        parts += context.getStringFmt(
            R.string.settings_recovery_kit_review_remove_done_here,
            who, eviction.evicted.size,
        )
    } else {
        parts += context.getStringFmt(R.string.settings_recovery_kit_review_remove_none_here, who)
    }
    val folders = eviction.unreachable.count { it.`class` == UnreachableSeatClass.FOLDER_CHANNEL }
    if (folders > 0) {
        parts += context.getStringFmt(R.string.settings_recovery_kit_review_remove_folder_seats, folders)
    }
    val unsynced = eviction.unreachable.count { it.`class` == UnreachableSeatClass.CHAT_GROUP_NO_THREAD_HERE }
    if (unsynced > 0) {
        parts += context.getStringFmt(R.string.settings_recovery_kit_review_remove_unsynced_seats, unsynced)
    }
    return parts.joinToString(" ")
}
