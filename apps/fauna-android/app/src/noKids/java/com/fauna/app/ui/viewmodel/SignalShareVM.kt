package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.feed.FeedManagerHost
import com.fauna.ffi.FfiReportShareEntry
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
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
 * withdraws this actor's contributed rows, so the published list can shrink),
 * and treats a `null` manager (pre-auth; the page can mount before the WS
 * socket is up) as a silent no-op on hydrate, not an error — the next
 * page-visible [refresh] re-hydrates once the manager exists.
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

    /**
     * Read the opt-in + transparency list over the shared `FfiFeedManager`.
     * Call on entering the Personalization route AND on every re-entry (the
     * android idiom for "page visible" — `LaunchedEffect(Unit)` re-fires on
     * each navigation back to the destination) since the published list
     * changes out-of-band: another contributor on this nest crossing k=3, or
     * this actor opting out on another device. A `null` manager is a no-op
     * (pre-auth). `signalShareStatus` is a single NestClient RPC (plus a
     * trivial local cache write) — the transport already parks it while the
     * socket comes up (transport.md § Request lifecycle step 3).
     */
    fun refresh() {
        val manager = feedManagerHost.manager() ?: return
        viewModelScope.launch {
            try {
                val status = manager.signalShareStatus()
                _share.value = status.share
                _published.value = status.published
                _errorMessage.value = null
            } catch (e: Exception) {
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
     */
    fun setShare(share: Boolean) {
        val manager = feedManagerHost.manager()
        if (manager == null) {
            _errorMessage.value = appContext.getString(R.string.common_not_connected)
            return
        }
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
}
