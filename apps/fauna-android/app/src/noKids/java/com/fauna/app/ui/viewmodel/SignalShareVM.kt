package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.feed.FeedManagerHost
import com.fauna.app.ui.util.answerRefusal
import com.fauna.ffi.FfiReportShareEntry
import com.fauna.ffi.shareSignalsPossibility
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_core.Possibility
import uniffi.fauna_core.ReadState
import javax.inject.Inject

/**
 * Drives the Personalization home's Layer-B **signal-sharing** pane —
 * `personalization-share-signals-toggle` + the `signal-share-published-list`
 * transparency pane (`engagement-cues.md` § Layer B). Mirrors linux's
 * `SignalShareCtx`/`wire_signal_sharing` (`apps/fauna-linux/src/views/personalization/mod.rs`):
 * reads/writes over the shared `FfiFeedManager` (**not** `FfiModerationClient`
 * directly — the manager caches the opt-in for its own producer and is the
 * shared-Rust boundary per priority #2), never renders optimistically (a
 * `set` result comes back from the nest and IS the new state — opting out
 * withdraws this actor's contributed rows, so the published list can shrink).
 *
 * **The toggle waits for what it acts through** (`engagement-cues.md` § Layer B;
 * `docs/goal/ui/README.md` § Offer only what is possible, rules 5–7): it is
 * painted from [shareAnswer] — the shared
 * `fauna_client_personalization::engagement::share_signals_possibility` over
 * whether the live feed manager is up and where the `signal_share.status` read
 * stands — and [toggleShare] asks the same answer again, refusing on
 * `error-message` instead of writing a guess or returning without a word. It
 * writes only the flip of the nest-confirmed value, never a value the switch
 * painted before the read.
 *
 * No echo-suppression flag is needed the way GTK needs one: Compose's
 * `Switch(checked = ..., onCheckedChange = ...)` only invokes the callback
 * from a user gesture, never from a `checked` value change driven by
 * recomposition, so rendering [share] straight from the nest-confirmed reply
 * cannot re-fire [setShare].
 */
@HiltViewModel
class SignalShareVM @Inject constructor(
    private val feedManagerHost: FeedManagerHost,
    @ApplicationContext private val appContext: Context,
) : ViewModel() {

    private val _share = MutableStateFlow(false)
    val share: StateFlow<Boolean> = _share.asStateFlow()

    private val _published = MutableStateFlow<List<FfiReportShareEntry>>(emptyList())
    val published: StateFlow<List<FfiReportShareEntry>> = _published.asStateFlow()

    private val _errorMessage = MutableStateFlow<String?>(null)
    val errorMessage: StateFlow<String?> = _errorMessage.asStateFlow()

    /** Where the `signal_share.status` read stands: until it answers, [share]
     *  holds only the pre-read default, which the toggle must never flip. */
    private var read = ReadState.PENDING

    private val _shareAnswer = MutableStateFlow(answerNow())

    /** `personalization-share-signals-toggle`'s answer — what the switch is
     *  painted from and [toggleShare] asks again. */
    val shareAnswer: StateFlow<Possibility> = _shareAnswer.asStateFlow()

    /** The shared answer over the live manager and the read, asked now. */
    private fun answerNow(): Possibility =
        shareSignalsPossibility(feedManagerHost.manager() != null, read)

    private fun repaint() {
        _shareAnswer.value = answerNow()
    }

    /** Fold a status read: [status] the reply, or `null` when it threw. A
     *  failed read leaves the opt-in unknown — unless an earlier read
     *  answered, whose confirmed value stays the toggle's. Internal so the VM
     *  tests drive it without a nest. */
    internal fun applyRead(status: com.fauna.ffi.FfiReportShareStatus?) {
        if (status != null) {
            _share.value = status.share
            _published.value = status.published
            read = ReadState.RESOLVED
        } else if (read != ReadState.RESOLVED) {
            read = ReadState.FAILED
        }
        repaint()
    }

    /**
     * Read the opt-in + transparency list over the shared `FfiFeedManager`.
     * Call on entering the Personalization route AND on every re-entry (the
     * android idiom for "page visible" — `LaunchedEffect(Unit)` re-fires on
     * each navigation back to the destination) since the published list
     * changes out-of-band: another contributor on this nest crossing k=3, or
     * this actor opting out on another device. The page can mount before the
     * WS socket is up, so a missing manager is waited for like the sibling
     * Settings pages' hydrate loops, the toggle painted *the feed isn't ready
     * yet* meanwhile. `signalShareStatus` is a single NestClient RPC (plus a
     * trivial local cache write) — the transport already parks it while the
     * socket comes up (transport.md § Request lifecycle step 3).
     */
    fun refresh() {
        repaint()
        viewModelScope.launch {
            var manager = feedManagerHost.manager()
            var attempt = 1
            while (manager == null && attempt < HYDRATE_ATTEMPTS) {
                delay(HYDRATE_RETRY_MS)
                manager = feedManagerHost.manager()
                attempt++
            }
            if (manager == null) {
                repaint()
                return@launch
            }
            try {
                applyRead(manager.signalShareStatus())
                _errorMessage.value = null
            } catch (e: Exception) {
                applyRead(null)
                _errorMessage.value = e.message
            }
        }
    }

    /**
     * Flip the opt-in (`fauna.moderation.signal_share.set` via the manager),
     * then render straight from the nest-confirmed reply — never
     * optimistically. A false→true flip opts this device's derived cue
     * verdicts on public posts into the network aggregate; true→false ALSO
     * withdraws every `signal:*` row this actor contributed (the nest
     * opt-out sweep), so [published] may shrink on this same re-read.
     *
     * Asks [shareAnswer]'s predicate again first: no live manager, or an
     * unread or failed status read, refuses with the answer's reason on
     * `error-message` and sends nothing. The value written is the flip of the
     * nest-confirmed [share], whatever the switch last painted.
     */
    fun toggleShare() {
        val manager = feedManagerHost.manager()
        val answer = shareSignalsPossibility(manager != null, read)
        _shareAnswer.value = answer
        val refusal = answerRefusal(appContext, answer)
        if (refusal != null || manager == null) {
            refusal?.reason?.let { _errorMessage.value = it }
            return
        }
        val share = !_share.value
        viewModelScope.launch {
            try {
                val status = manager.setSignalSharing(share)
                _share.value = status.share
                _published.value = status.published
                _errorMessage.value = null
            } catch (e: Exception) {
                _errorMessage.value = e.message
            }
        }
    }

    private companion object {
        const val HYDRATE_ATTEMPTS = 10
        const val HYDRATE_RETRY_MS = 500L
    }
}
