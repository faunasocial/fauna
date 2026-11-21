package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.ui.util.pressAsked
import com.fauna.ffi.FfiReportShareEntry
import com.fauna.ffi.FfiReportShareStatus
import com.fauna.ffi.readPossibility
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.MailSpamAction
import uniffi.fauna_client_mail_settings.MailSpamMachine
import uniffi.fauna_client_mail_settings.MailSpamSnapshot
import uniffi.fauna_client_mail_settings.SpamStatus
import uniffi.fauna_core.Possibility
import uniffi.fauna_core.ReadState
import javax.inject.Inject

/**
 * Renders the shared `MailSpamMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the per-account `mail-spam` page — reset the per-user Bayesian
 * model, opt in/out of the deployment baseline, and undo individual training
 * events. Per priority #2 this view-model holds **no** spam logic; it owns the
 * machine, mirrors its snapshot into a StateFlow, and dispatches actions. The
 * per-user feedback loop is not yet built nest-side (`mail-spam.md` § Impl status
 * today), so every action surfaces the seam's `unimplemented` rejection via
 * `snapshot.error` → the global error-message banner; the page is never faked
 * green.
 *
 * Distributed report sharing (`report-sharing.md` § Client wire) rides a
 * separate, already-landed `FfiModerationClient` seam — a bool opt-in plus a
 * read-only transparency list, not a state machine — so it gets its own
 * `reportShare`/`reportSharePublished` flows instead of folding into the
 * `MailSpamMachine` snapshot. Linux lead: apps/fauna-linux/src/settings/mail_spam.rs.
 */
@HiltViewModel
class MailSpamVM @Inject constructor(
    @ApplicationContext private val context: Context,
    private val api: ApiClient,
) : ViewModel() {

    private val machine: MailSpamMachine? = api.buildMailSpamMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    // Distributed report-sharing (report-sharing.md § Client wire) is a small
    // dedicated flow over FfiModerationClient directly — NOT part of the
    // MailSpamMachine (it's a bool + a read-only transparency list, not a state
    // machine; mirrors linux's separate hydrate_report_share/set_report_share).
    val reportShare = MutableStateFlow(false)
    val reportSharePublished = MutableStateFlow<List<FfiReportShareEntry>>(emptyList())

    // Per-account spam-threshold override — another small dedicated flow over a plain RPC
    // pair, not the MailSpamMachine, same shape as report-share above. `null`
    // = follows the admin default; `0u` is a real setting, distinct from `null`.
    val thresholdOverride = MutableStateFlow<UInt?>(null)

    // Controls wait for their reads (mail-settings.md § Errors & edge cases,
    // *Controls wait for their reads*; tui's `contribute_answer` /
    // `share_read` / `threshold_read`): where the classifier snapshot, the
    // report-share read and the override read stand, held beside the reads
    // they track. Each control's answer is the shared
    // `Possibility::from_read` over its read, so a pre-read default (`false`,
    // an empty override) is never written as a guess.
    private var classifierRead = ReadState.PENDING
    private var shareRead = ReadState.PENDING
    private var thresholdRead = ReadState.PENDING
    private val _contributeAnswer = MutableStateFlow(readPossibility(ReadState.PENDING))
    private val _shareAnswer = MutableStateFlow(readPossibility(ReadState.PENDING))
    private val _thresholdAnswer = MutableStateFlow(readPossibility(ReadState.PENDING))

    /** `mail-spam-contribute-baseline-toggle`: the classifier snapshot's read. */
    val contributeAnswer: StateFlow<Possibility> = _contributeAnswer.asStateFlow()

    /** `mail-spam-share-reports-toggle`: the report-share read. */
    val shareAnswer: StateFlow<Possibility> = _shareAnswer.asStateFlow()

    /** `mail-spam-threshold-override-input` and its commit: the override read. */
    val thresholdAnswer: StateFlow<Possibility> = _thresholdAnswer.asStateFlow()

    /** Rule 6 at the press: ask [answer] again; a blocked one names its
     *  reason on `error-message` and sends nothing. */
    private inline fun asked(answer: Possibility, go: () -> Unit) =
        pressAsked(context, answer, onRefuse = { errorMessage.value = it }, go = go)

    init {
        hydrate()
        hydrateReportShare()
        hydrateThresholdOverride()
        // The history open here follows an undo or a reset on another device
        // (`StaleSurfaces::mail_spam`, mail-spam.md § Undo step 4, § Reset
        // step 6), and a reconnect re-reads it too — a push sent while the
        // socket was down is never replayed. Only the machine's own hydrate:
        // the reset confirm's armed state lives in the screen, which a
        // snapshot never touches, so a push never disarms it.
        viewModelScope.launch { api.mailSpamChangedTick.collect { hydrate() } }
        viewModelScope.launch { api.reconnectTick.collect { hydrate() } }
    }

    /** Load training history + the contribution flag; retry while the socket
     *  comes up (mirrors [LinkedNestsVM]). */
    private fun hydrate() {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.hydrate()
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that has not landed yet
                // (NestClient::request_inner), and the machine records any real failure
                // in its own snapshot, which the publish below surfaces.
            }
            publish(m)
            // The machine records a failed hydrate in its snapshot's `error`.
            classifierRead = if (m.snapshot().error == null) ReadState.RESOLVED else afterFailedRead(classifierRead)
            _contributeAnswer.value = readPossibility(classifierRead)
        }
    }

    fun resetModel() = dispatch(MailSpamAction.ResetModel)

    /** Asks the toggle's answer first — once the machine exists: a
     *  never-built machine is [dispatch]'s unbuilt refusal (rule 7), not a
     *  "still loading" that would never end. */
    fun setContributeBaseline(contribute: Boolean) {
        val action = MailSpamAction.SetContributeBaseline(contribute = contribute)
        if (machine == null) return dispatch(action)
        asked(_contributeAnswer.value) { dispatch(action) }
    }

    fun undo(historyIdHex: String) =
        dispatch(MailSpamAction.UndoTraining(historyIdHex = historyIdHex))

    /** Load the report-share opt-in + published transparency list. A single
     *  NestClient RPC — the transport already parks it while the socket
     *  comes up (transport.md § Request lifecycle step 3). */
    private fun hydrateReportShare() {
        viewModelScope.launch {
            try {
                publishReportShare(api.reportShareStatus())
                shareRead = ReadState.RESOLVED
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that
                // has not landed yet (NestClient::request_inner).
                shareRead = afterFailedRead(shareRead)
            }
            _shareAnswer.value = readPossibility(shareRead)
        }
    }

    /** Set the report-share opt-in, then re-read status so the toggle + published
     *  list reflect the persisted value (opting out withdraws this actor's
     *  reports, which may shrink the list — mirrors linux `set_report_share`). */
    fun setReportShare(share: Boolean) = asked(_shareAnswer.value) {
        viewModelScope.launch {
            try {
                api.reportShareSet(share)
                publishReportShare(api.reportShareStatus())
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    private fun publishReportShare(status: FfiReportShareStatus) {
        reportShare.value = status.share
        reportSharePublished.value = status.published
    }

    /** Load the threshold override. A single NestClient RPC — the transport
     *  already parks it while the socket comes up (transport.md § Request
     *  lifecycle step 3). */
    private fun hydrateThresholdOverride() {
        viewModelScope.launch {
            try {
                thresholdOverride.value = api.spamThresholdOverrideGet()
                thresholdRead = ReadState.RESOLVED
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that
                // has not landed yet (NestClient::request_inner).
                thresholdRead = afterFailedRead(thresholdRead)
            }
            _thresholdAnswer.value = readPossibility(thresholdRead)
        }
    }

    /** Set (or clear, with `null`) the override, then reflect the persisted
     *  value the nest confirmed — never the local edit (mirrors
     *  [setReportShare]). Asks the override read's answer first, so an empty
     *  pre-read draft is never committed as "clear the override". */
    fun setThresholdOverride(value: UInt?) = asked(_thresholdAnswer.value) {
        viewModelScope.launch {
            try {
                thresholdOverride.value = api.spamThresholdOverrideSet(value)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    private fun dispatch(action: MailSpamAction) {
        // Rule 7 (ui/README.md § Offer only what is possible): a gesture whose
        // machine was never built says so on `error-message`, never silently.
        val m = machine ?: run {
            errorMessage.value = api.mailUnbuiltRefusal()
            return
        }
        viewModelScope.launch {
            try {
                m.dispatch(action)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            publish(m)
        }
    }

    private fun publish(m: MailSpamMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
    }

    private companion object {
        /** A read that failed keeps an earlier success (`Possibility::from_read`'s
         *  rule; tui's `read_failed`): the page still holds a value the nest
         *  confirmed. */
        fun afterFailedRead(was: ReadState): ReadState =
            if (was == ReadState.RESOLVED) ReadState.RESOLVED else ReadState.FAILED

        val EMPTY_SNAPSHOT = MailSpamSnapshot(
            events = emptyList(),
            contributeBaseline = false,
            status = SpamStatus.IDLE,
            error = null,
        )
    }
}
