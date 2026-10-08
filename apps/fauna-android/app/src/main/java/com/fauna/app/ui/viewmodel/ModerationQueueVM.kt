package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ReportSheetStore
import com.fauna.ffi.FfiReportLedgerRow
import com.fauna.ffi.moderationQueue
import com.fauna.ffi.reportWithdrawVerdict
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

    init {
        load()
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

    /** Server-row correction — the 1d surface switch (mail-spam.md § Encrypted-mode
     *  interaction; co-design § Revision history 2026-07-06): when the nest advertises
     *  `spam-model-sealed-at-rest` and mail is enabled, train the sealed model
     *  client-side over the post body (unwrap → mutate → re-seal → `put_spam_model`),
     *  else degrade to server-side `fauna.moderation.train` (mirrors linux
     *  `train_moderation_flow`). */
    private suspend fun trainServerRow(contentId: String) {
        if (api.sealedSpamWriteAvailable()) {
            // A fetch/decode miss falls through to the server train (linux's
            // `if let Ok(reply)`); a sealed-write failure surfaces (rethrows).
            val body = try {
                api.postBodyText(contentId)
            } catch (_: Exception) {
                null
            }
            if (!body.isNullOrEmpty()) {
                if (api.trainSpamModelClient(body, false)) return // "ham"
                // ServerPath race (e.g. mail disabled mid-flow) → fall through.
            }
        }
        api.submitModerationTrain(contentId, "ham")
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
