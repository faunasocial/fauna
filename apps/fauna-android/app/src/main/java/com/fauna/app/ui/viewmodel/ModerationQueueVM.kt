package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.PendingAppealStore
import com.fauna.app.core.ReportSheetStore
import com.fauna.ffi.FfiReportLedgerRow
import com.fauna.ffi.appealFormView
import com.fauna.ffi.appealVerdict
import com.fauna.ffi.moderationQueue
import com.fauna.ffi.reportWithdrawVerdict
import com.fauna.ffi.rowIsAppealable
import com.fauna.ffi.shortId
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_moderation.QueueRow
import uniffi.fauna_core.LocalizedText
import javax.inject.Inject

/**
 * Drives the standalone **Moderation queue** page ([ModerationQueueScreen]) — the
 * user's view onto why their *own* content was labeled / quarantined / rejected, and
 * their lever to correct it (moderation.md § Goal). Mirrors linux `views/moderation.rs`
 * + `client.rs::{fetch_moderation_actions, correct_moderation_row}` and the windows
 * `ModerationViewModel`, over the same shared seams (priority #2/#3).
 *
 * The queue is the **union** of two sources (moderation.md § Layout & flow): the nest's
 * server `ObligationAction`s ([ApiClient.moderationActions], `fauna.moderation.actions`)
 * ∪ the client's own post-decrypt local detections ([ApiClient.moderationLocalDetections],
 * the encrypted-mode social signal the nest can't see), merged + deduped by `content_id`
 * (server row wins) via the shared UniFFI [moderationQueue] façade. Holds no
 * authoritative moderation state — a pure projection (moderation.md § Persistence).
 */
@HiltViewModel
class ModerationQueueVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    /** One merged [QueueRow] per row — server obligation rows carry an enforcement
     *  `action`, local detections a blank action column. Empty `[]` is the empty
     *  state, not an error. */
    val queue = MutableStateFlow<List<QueueRow>>(emptyList())
    val isLoading = MutableStateFlow(false)
    val errorMessage = MutableStateFlow<String?>(null)

    /** The reporter's own ledger — every abuse report this user filed, newest first
     *  (`moderation-reports-section`; moderation.md § User-initiated reporting →
     *  *What the reporter is told*), already worded by shared Rust. */
    val reports = MutableStateFlow<List<FfiReportLedgerRow>>(emptyList())

    /** Whether the ledger has been read at least once — the empty line paints only
     *  off this bit, so a slow first read never claims "You have not reported
     *  anything" (`ui/README.md` § List pages: loading is not empty). */
    val reportsLoaded = MutableStateFlow(false)

    /** The line the last withdraw painted (`null` before one). */
    val reportStatus = MutableStateFlow<LocalizedText?>(null)

    /** The appeal the user is composing (moderation.md § Legal takedown — the
     *  transparency triple's second leg). At most one is open at a time. */
    data class AppealDraft(val contentId: String, val reason: String = "")

    /** The open appeal form, or `null`. It captures its row's `content_id` at OPEN
     *  time — the queue refetches, so a dispatch keyed on the row index could
     *  appeal the wrong decision. */
    val appealDraft = MutableStateFlow<AppealDraft?>(null)

    /** The last appeal's outcome line (`appeal-status`). Deliberately NOT
     *  [errorMessage]: a refused appeal is form feedback, and routing it to the
     *  page-level banner would blank the queue the user is reading. */
    val appealStatus = MutableStateFlow<LocalizedText?>(null)

    private val appealBusy = MutableStateFlow(false)

    init {
        load()
    }

    fun openAppeal(contentId: String) {
        appealDraft.value = AppealDraft(contentId)
        appealStatus.value = null
    }

    /** Open the appeal a `dm-message-withheld-badge` tap asked for ([PendingAppealStore]), on
     *  the loaded row it names — behind the same [rowIsAppealable] gate as the row's own
     *  `appeal-button`. A row the queue no longer carries opens nothing. */
    private fun openPendingAppeal() {
        val contentId = PendingAppealStore.consume() ?: return
        val row = queue.value.firstOrNull { it.contentId == contentId } ?: return
        if (rowIsAppealable(row)) openAppeal(contentId)
    }

    fun cancelAppeal() {
        appealDraft.value = null
        appealStatus.value = null
    }

    fun setAppealReason(reason: String) {
        appealDraft.value = appealDraft.value?.copy(reason = reason)
    }

    /** Dispatch the open appeal. Success closes the form (the decision is with an
     *  admin now; the queue row stays — appeals are additive history); a failure
     *  KEEPS it open so the reason can be fixed and resent. The shared fold gates
     *  the dispatch, so a reasonless appeal never reaches the wire. */
    fun submitAppeal() {
        val draft = appealDraft.value ?: return
        if (!appealFormView(draft.contentId, draft.reason).canSubmit) return
        if (!appealBusy.compareAndSet(false, true)) return
        viewModelScope.launch {
            try {
                api.moderationAppeal(draft.contentId, draft.reason.trim())
                appealDraft.value = null
                appealStatus.value = appealVerdict(null)
            } catch (e: Exception) {
                appealStatus.value = appealVerdict(e.message ?: e.toString())
            } finally {
                appealBusy.value = false
            }
        }
    }

    /** Read the reporter's ledger (`fauna.moderation.abuse_report.mine`). A failed
     *  read never hides the queue rows beside it — it only keeps the loaded bit down. */
    fun loadReports() {
        viewModelScope.launch {
            try {
                reports.value = api.abuseReportMine()
                reportsLoaded.value = true
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    /** Withdraw one open report, then re-read the ledger. The verdict line is the
     *  shared fold's (success names what was deleted everywhere). */
    fun withdrawReport(reportId: String) {
        viewModelScope.launch {
            reportStatus.value = try {
                api.abuseReportWithdraw(reportId)
                reportWithdrawVerdict(null)
            } catch (e: Exception) {
                reportWithdrawVerdict(e.message ?: e.toString())
            }
            loadReports()
        }
    }

    /** Load the queue: fetch the server rows from `fauna.moderation.actions` (a
     *  single NestClient RPC — the transport already parks it while the socket
     *  comes up, transport.md § Request lifecycle step 3), read the session's
     *  local detections, and merge the two via the shared [moderationQueue].
     *  An empty reply renders the empty state, not an error (moderation.md §
     *  Errors & edge cases). */
    fun load() {
        viewModelScope.launch {
            isLoading.value = true
            try {
                val server = api.moderationActions()
                val local = api.moderationLocalDetections()
                queue.value = moderationQueue(server, local)
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that
                // has not landed yet (NestClient::request_inner).
            }
            isLoading.value = false
            openPendingAppeal()
            // The reporter's ledger rides every Moderation entry beside the queue —
            // its own failure never hides the queue rows above.
            loadReports()
        }
    }

    /**
     * Apply a `train-correction-button` click for one queue row (`verdict:"ham"` — the
     * queued item was *flagged*, so the correction is "false positive → not spam";
     * mirrors linux `correct_moderation_row`).
     *
     * A **server** row (carries an enforcement `action`) trains via the 1d sealed-write
     * switch ([trainServerRow]); the row stays (enforcement history). A **local** row
     * (a client-side post-decrypt detection, blank action) has no nest obligation to
     * train against — the content is MLS-sealed at rest — so the correction removes the
     * false-positive flag from the session store and repaints, additionally feeding the
     * ham correction to the client-side sealed model when the write path is available.
     */
    fun correct(contentId: String, isLocal: Boolean) {
        viewModelScope.launch {
            try {
                if (isLocal) {
                    // Read the retained plaintext BEFORE dropping the flag, then remove
                    // + repaint (the removal is the correction), then best-effort feed
                    // the ham correction to the sealed model.
                    val body = api.moderationMessageBody(contentId)
                    api.moderationRemoveLocalDetection(contentId)
                    load()
                    if (body != null) trainSpamModelClientSide(body)
                } else {
                    trainServerRow(contentId)
                }
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    /** Feed one ham correction to the **client-side** sealed tier-1 model for text the
     *  client already holds (a local detection's retained body). Silent unless it fails
     *  — for client-only content there is no server row to fall back to (the nest can't
     *  read it), exactly as the flag-removal alone was the correction before the 1d
     *  write path (mirrors linux `train_spam_model_client_side`). */
    private suspend fun trainSpamModelClientSide(text: String) {
        api.trainSpamModelClient(text, false) // is_spam = false (ham); ServerPath no-ops here
    }

    /** Server-row correction — the one shared flow (mail-spam.md § Encrypted-mode
     *  interaction; moderation.md § State & data shape → *Training correction*):
     *  the sealed model write when possible, then `fauna.moderation.train` always,
     *  so every correction reports. A failure surfaces (rethrows) to [correct]. */
    private suspend fun trainServerRow(contentId: String) {
        api.trainModerationCorrection(contentId, false) // "ham" — a false positive
    }

    companion object {
        /**
         * One ledger row's line — `reason · status · short subject id · outcome — where
         * it went` (web's `ledgerLine`, tui's `ledger_elements`, apple's
         * `ledgerLine`: the e2e reads this text on every app). [resolve] maps a shared
         * [LocalizedText] to the user's words.
         */
        fun ledgerLine(row: FfiReportLedgerRow, resolve: (LocalizedText) -> String): String {
            val parts = mutableListOf(resolve(row.reason), resolve(row.status))
            ReportSheetStore.subjectId(row.subject)?.let { parts.add(shortId(it)) }
            row.outcome?.let { parts.add(resolve(it)) }
            return "${parts.joinToString(" · ")} — ${resolve(row.routedTo)}"
        }

        /** Whole-percent confidence from the wire's per-mille `u16` (0–1000; the
         *  dag-cbor wire forbids floats), rounded half-up. Single-sourced in shared
         *  `fauna_core::format::confidence_percent` (value-formatting.md § Confidence
         *  percent) — delegates so the rounding can't drift per-app. */
        fun confidencePercent(perMille: UShort): Int = com.fauna.ffi.confidencePercent(perMille).toInt()
    }
}
