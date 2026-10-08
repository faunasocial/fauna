package com.fauna.app.core

import com.fauna.ffi.FfiReportForm
import com.fauna.ffi.FfiReportSubject
import com.fauna.ffi.FfiReportTarget
import com.fauna.ffi.reportFailed
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import uniffi.fauna_core.LocalizedText

/**
 * The state behind the one shared report sheet and its acknowledgement line
 * (`report-sheet`, `report-status` — `moderation.md` § User-initiated reporting →
 * *App surface*): the **android twin** of apple's `ReportSheetStore`, web's
 * `ReportHost.svelte` and tui's `report.rs` (priority #2/#3). One sheet for the
 * three verbs (a feed post's ⋯, a conversation message's ⋯, an OTHER profile),
 * so one store: a verb calls [open] with the target the shared Rust constructors
 * built (`reportPostTarget` / `reportMessageTarget` / `reportActorTarget` — they
 * carry the sealed rule once), and [com.fauna.app.ui.components.ReportHost]
 * paints whatever is open.
 *
 * Every decision is shared Rust's: the reason list, the submit gate, the
 * include-text rule and the words come from `reportSheetView(target, form)`
 * (folded by the host per keystroke), and the request is built from the sheet by
 * the shared `report_request` inside `abuseReportSubmit`. This type only
 * sequences the follow-ups, exactly as the references do: submit →
 * `knocks_block(author)` when ticked → the reporter-side hide (`hideReported`) →
 * the stored list into the render inputs ([onHiddenContent]). A failed send
 * keeps the sheet open; a failed block/hide lands beside the acknowledgement —
 * never silent.
 *
 * Held by [ContentPolicyStore] (the reporter-side hide list is that store's
 * input), so a report filed from a card the hide then replaces still has
 * somewhere to paint its acknowledgement.
 */
class ReportSheetStore(
    private val api: ApiClient,
    /** The stored hide list after `hideReported` — the whole truth, replaces the cache. */
    private val onHiddenContent: (List<String>) -> Unit,
) {
    /**
     * One immutable snapshot. FFI-free by construction (the per-keystroke
     * `reportSheetView` fold is the host's), so the store's sequencing is
     * testable without the native library.
     */
    data class State(
        /** The subject the opening verb chose; `null` is a closed sheet. */
        val target: FfiReportTarget? = null,
        val form: FfiReportForm = EMPTY_FORM,
        val sending: Boolean = false,
        /** The acknowledgement — present only after a send landed, until the next open. */
        val status: LocalizedText? = null,
        /** A failed send's words (shared `reportFailed`), painted on `error-message`. */
        val error: LocalizedText? = null,
        /** A failed block/hide AFTER a landed send — beside [status], not instead of it. */
        val followupError: String? = null,
    )

    private val _state = MutableStateFlow(State())
    val state: StateFlow<State> = _state.asStateFlow()

    /** Open the sheet on [target] from an empty draft with no stale acknowledgement or error. */
    fun open(target: FfiReportTarget) {
        _state.value = State(target = target)
    }

    fun cancel() {
        _state.update { it.copy(target = null) }
    }

    /** Edit the draft; the host folds the new sheet view from it. */
    fun edit(transform: (FfiReportForm) -> FfiReportForm) {
        _state.update { it.copy(form = transform(it.form)) }
    }

    /**
     * Drop everything — the departing account's draft and acknowledgement must
     * never reach the next one (`account-scoping.md` § The scoping taxonomy).
     */
    fun reset() {
        _state.value = State()
    }

    /** Send the report and run the follow-ups. The host only enables it when the shared view says it may send. */
    suspend fun submit() {
        val current = _state.value
        val sent = current.target ?: return
        if (current.sending) return
        _state.update { it.copy(sending = true) }
        val draft = current.form
        val reply = try {
            api.abuseReportSubmit(sent, draft)
        } catch (e: Exception) {
            // The sheet stays open for a retry.
            _state.update { it.copy(sending = false, error = reportFailed(e.message ?: e.toString())) }
            return
        }
        var followup: String? = null
        val author = sent.author
        if (draft.blockAuthor && author != null) {
            try {
                api.blockKnock("", author)
            } catch (e: Exception) {
                followup = "block: ${e.message ?: e}"
            }
        }
        subjectId(sent.subject)?.let { id ->
            try {
                onHiddenContent(api.hideReported(id))
            } catch (e: Exception) {
                followup = followup ?: "hide: ${e.message ?: e}"
            }
        }
        _state.update {
            it.copy(
                target = null,
                sending = false,
                status = reply.acknowledgement,
                error = null,
                followupError = followup,
            )
        }
    }

    companion object {
        val EMPTY_FORM = FfiReportForm(reason = null, note = "", includeText = false, blockAuthor = false)

        /**
         * The id the reporter-side hide keys on: a post's or message's record id,
         * or an account's actor id (`hide_reported`'s contract); `null` for a
         * subject kind this build cannot read.
         */
        fun subjectId(subject: FfiReportSubject): String? = when (subject) {
            is FfiReportSubject.Post -> subject.cid
            is FfiReportSubject.Message -> subject.recordCid
            is FfiReportSubject.Actor -> subject.actorId
            is FfiReportSubject.Unknown -> null
        }
    }
}
