package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.FeedTriple
import com.fauna.app.core.ShellLog
import com.fauna.ffi.FfiBridgeFollow
import com.fauna.ffi.FfiBridgeStatus
import com.fauna.ffi.FfiCborEntry
import com.fauna.ffi.FfiCborValue
import com.fauna.ffi.FfiException
import com.fauna.ffi.FfiFamilyFeedRequest
import com.fauna.ffi.FfiFeedSourceOperation
import com.fauna.ffi.feedSourceOperationWire
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class BridgesVM @Inject constructor(
    private val api: ApiClient,
    @ApplicationContext private val context: Context,
) : ViewModel() {

    // ── The ward's feed-source asks (family-safety.md § Feed-source approvals) ──
    //
    // `bridge-source-request-button` / `bridge-source-request-state` inside
    // `bridge-card`, lifted from tui's `bridges::source_ask_rows` (linux:
    // `views/bridges/detail.rs`). Two inputs, both supervised-only by
    // construction (so no "is supervised" test anywhere — rule (h)): the
    // durable `status.feed_requests` and this session's TYPED refusals, both
    // held in the app-wide [com.fauna.app.core.WardAsks] so the AT Protocol page's
    // embedded card and this page read the same lists.

    /** The ward's durable own feed-source asks. */
    val feedAsks: StateFlow<List<FfiFamilyFeedRequest>> = api.wardAsks.feedRequests

    /** The triples this session saw refused by the guardian gate. */
    val refusedFeed: StateFlow<List<FeedTriple>> = api.wardAsks.refusedFeed

    /**
     * Split the guardian gate off every other link / follow failure on the
     * exception TYPE (the shared `RpcError::is_guardian_approval_required`,
     * routed to `FfiException.GuardianApprovalRequired`) — tui's
     * `bridges::guardian_gate_or_failed`, linux's `feed_source_refusal`.
     * Offering the ask on a transport failure would tell an unsupervised user
     * their account is supervised (rule (a)). The refusal STAYS on
     * `error-message` (rule (b)); it just stops being a dead end.
     */
    private fun failed(e: Exception, refused: FeedTriple) {
        if (e is FfiException.GuardianApprovalRequired) {
            api.wardAsks.noteFeedRefusal(refused)
            _error.value = context.getString(R.string.bridges_source_blocked)
        } else {
            _error.value = e.message
        }
    }

    /**
     * `bridge-source-request-button` — ask the guardian for one refused
     * triple (`fauna.family.feed_source.request`). On success the nest's
     * re-read lands in the durable store (the button swaps for its state) and
     * the refusal comes off `error-message`; on failure the ask's own typed
     * refusal is the ward's to read verbatim. Never retries the original
     * operation — an approval is a single-use grant the ward redeems by
     * repeating it (rule (e)).
     */
    fun requestFeedSource(triple: FeedTriple) {
        viewModelScope.launch {
            try {
                // Display-only: the add-follow petname is gone by now (the row
                // cleared at dispatch), so the bridge id is the one label still
                // available — nothing is invented.
                api.familyFeedSourceRequest(triple, triple.bridgeId)
                _error.value = null
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                _error.value = e.message ?: e.toString()
            }
        }
    }

    // The RAW, unfiltered list — a second instance of this VM backs the
    // AT Protocol page's Linked panel (docs/goal/ui/atproto.md § Layout & flow),
    // which needs to find the "bluesky" row even though the unified Bridges
    // PAGE must not list it. Filtering here at fetch time (rather than at the
    // Bridges page's own render) is the exact gotcha `atproto.md` § linux leg
    // warns about: it starved the shared snapshot and silently killed the
    // Bluesky notification-poll trigger, which reads the same reply. Apply
    // `isUnifiedBridgesPageBridge` at the PAGE (BridgesScreen), never here.
    private val _bridges = MutableStateFlow<List<FfiBridgeStatus>>(emptyList())
    val bridges: StateFlow<List<FfiBridgeStatus>> = _bridges

    private val _follows = MutableStateFlow<Map<String, List<FfiBridgeFollow>>>(emptyMap())
    val follows: StateFlow<Map<String, List<FfiBridgeFollow>>> = _follows

    private val _isLoading = MutableStateFlow(true)
    val isLoading: StateFlow<Boolean> = _isLoading

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error

    fun refresh() {
        viewModelScope.launch {
            _isLoading.value = true
            _error.value = null
            try {
                // Raw list, unfiltered — see the field doc above. Nostr/Bluesky
                // exclusion from the generic Bridges PAGE happens in
                // BridgesScreen's render, not here.
                val list = api.listBridges()
                _bridges.value = list
                val followsMap = mutableMapOf<String, List<FfiBridgeFollow>>()
                for (b in list) {
                    if (b.linked && b.supportsFollows) {
                        try {
                            followsMap[b.id] = api.listBridgeFollows(b.id)
                        } catch (e: Exception) {
                            ShellLog.w("BridgesVM", "list bridge follows failed for ${b.id}: ${e.message}")
                        }
                    }
                }
                _follows.value = followsMap
            } catch (e: Exception) {
                _error.value = e.message
            }
            _isLoading.value = false
        }
    }

    fun linkBridge(bridgeId: String, mode: String, fields: Map<String, String>, onRedirect: (String) -> Unit) {
        viewModelScope.launch {
            _error.value = null
            try {
                val resp = api.linkBridge(bridgeId, mode, fields)
                val redirect = resp.redirectUrl
                if (redirect != null) {
                    onRedirect(redirect)
                } else {
                    refresh()
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                // A `link` ask carries an EMPTY target by construction —
                // approving a link approves connecting that bridge
                // (`FeedSourceOperation::takes_target`).
                failed(e, FeedTriple(bridgeId, feedSourceOperationWire(FfiFeedSourceOperation.LINK), ""))
            }
        }
    }

    fun unlinkBridge(bridgeId: String) {
        viewModelScope.launch {
            _error.value = null
            try {
                api.unlinkBridge(bridgeId)
                refresh()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    fun updateSetting(bridgeId: String, key: String, value: FfiCborValue) {
        viewModelScope.launch {
            _error.value = null
            try {
                api.updateBridgeSettings(bridgeId, FfiCborValue.Map(listOf(FfiCborEntry(key, value))))
                refresh()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    fun addFollow(bridgeId: String, id: String, petname: String?) {
        viewModelScope.launch {
            _error.value = null
            try {
                api.addBridgeFollow(bridgeId, id, petname)
                refresh()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                failed(e, FeedTriple(bridgeId, feedSourceOperationWire(FfiFeedSourceOperation.FOLLOW), id))
            }
        }
    }

    fun removeFollow(bridgeId: String, followId: String) {
        viewModelScope.launch {
            _error.value = null
            try {
                api.removeBridgeFollow(bridgeId, followId)
                refresh()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }
}
