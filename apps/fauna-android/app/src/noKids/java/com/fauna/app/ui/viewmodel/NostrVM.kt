package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ShellLog
import com.fauna.app.payments.ZapSignerItem
import com.fauna.app.payments.nostrZapSignersAdd
import com.fauna.app.payments.nostrZapSignersList
import com.fauna.app.payments.nostrZapSignersRemove
import com.fauna.app.ui.util.and
import com.fauna.app.ui.util.answerRefusal
import com.fauna.app.ui.util.askForm
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiBridgeFollow
import com.fauna.ffi.FfiBridgeSetting
import com.fauna.ffi.FfiBridgeStatus
import com.fauna.ffi.FfiCborEntry
import com.fauna.ffi.FfiCborValue
import com.fauna.ffi.FfiCreateBunkerInviteReply
import com.fauna.ffi.FfiFeatureRow
import com.fauna.ffi.nostrAddFollowPossibility
import com.fauna.ffi.nostrAddRelayPossibility
import com.fauna.ffi.nostrLinkPossibility
import com.fauna.ffi.featuresGatePossibility
import com.fauna.ffi.nostrZapSignerAddPossibility
import com.fauna.ffi.readPossibility
import com.fauna.ffi.relayListAppending
import com.fauna.ffi.trimmedRelayInput
import uniffi.fauna_core.Possibility
import uniffi.fauna_core.ReadState
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject
import org.json.JSONArray
import com.fauna.app.ui.controls.Button

private const val NOSTR_BRIDGE_ID = "nostr"

/** The `zaps` plane's answer over the features read ([rows] `null` = not
 *  answered yet) — the shared gate, never a Kotlin re-derivation. */
internal fun zapsGateOver(rows: List<FfiFeatureRow>?): Possibility =
    featuresGatePossibility(rows, "zaps")

/**
 * The submit's second half for this page's forms (`docs/goal/ui/nostr.md`
 * § Errors & edge cases; README § Offer only what is possible, rule 4): ask
 * the shared predicate the button was painted from, and refuse with its reason
 * on `error-message` (`null` = the draft may go). Failing open like the paint.
 */
private fun refusalOf(context: Context, predicate: () -> Possibility): String? {
    val form = askForm(predicate)
    return if (form is Possibility.Blocked) resolveLocalized(context, form.reason) else null
}

// ── Nostr bridge-setting helpers (FFI-free — shared with NostrScreen's
// stateless Content composable for rendering). `relay_list` is a JSON array of
// relay URLs (nostr.md § Persistence); the 5 content flags are plain bools —
// both cross the wire as CBOR inside FfiBridgeStatus.settings.

/** Parse the `relay_list` setting (JSON array of relay URLs; absent/blank/
 * malformed all read as "no explicit list" rather than an error — the nest
 * publishes to its `DEFAULT_RELAYS` fallback in that case). */
internal fun parseRelayList(settings: List<FfiBridgeSetting>): List<String> {
    val raw = (settings.find { it.key == "relay_list" }?.value as? FfiCborValue.Text)?.v
    if (raw.isNullOrBlank()) return emptyList()
    return try {
        val arr = JSONArray(raw)
        (0 until arr.length()).map { arr.getString(it) }
    } catch (e: Exception) {
        emptyList()
    }
}

/** Encode relay URLs back to the JSON-array string the nest stores. */
internal fun encodeRelayList(urls: List<String>): String = JSONArray(urls).toString()

internal fun boolSetting(settings: List<FfiBridgeSetting>, key: String, default: Boolean): Boolean =
    (settings.find { it.key == key }?.value as? FfiCborValue.Bool)?.v ?: default

/**
 * Backs the standalone **Nostr** page (`docs/goal/ui/nostr.md`) — account
 * linking, content-publishing toggles, relay management, and follows, all over
 * the unified `fauna.bridges.*` control plane (`bridge_id:"nostr"`; the same
 * seam [BridgesVM] drives generically for every other bridge). Nostr keeps its
 * own dedicated page rather than being folded into the generic Bridges list
 * (ratified 2026-06-13), so this VM hardcodes the Nostr-specific fields
 * (link modes, the 5 content flags, the JSON `relay_list` setting) instead of
 * rendering [FfiBridgeStatus.settings] generically — mirrors the apple
 * `NostrVM` / linux `nostr_tab.rs` shape (priority #1/#3).
 */
@HiltViewModel
class NostrVM @Inject constructor(
    private val api: ApiClient,
    @ApplicationContext private val appContext: Context,
) : ViewModel() {

    /** False only when the bridge is absent from `fauna.bridges.list` entirely
     * (a nest built without the `nostr` cargo feature) — genuinely, permanently
     * unavailable. Distinct from [FfiBridgeStatus.available] (the per-user
     * nsec-deposit gate), which still renders the link form. */
    private val _registered = MutableStateFlow(true)
    val registered: StateFlow<Boolean> = _registered

    private val _bridge = MutableStateFlow<FfiBridgeStatus?>(null)
    val bridge: StateFlow<FfiBridgeStatus?> = _bridge

    private val _follows = MutableStateFlow<List<FfiBridgeFollow>>(emptyList())
    val follows: StateFlow<List<FfiBridgeFollow>> = _follows

    // Connected apps invite start (NIP-46 bunker, nostr.md § The nest as the
    // user's NIP-46 signer). [bunkerInvite] holds the one-time connect-string
    // reveal from the most recent mint — cleared only by a fresh mint or
    // navigating away, matching the web/linux precedent (the string is shown
    // once, never re-fetchable). The roster of connections is the Connected apps
    // page's (connected-apps.md), not this page's.
    private val _bunkerInvite = MutableStateFlow<FfiCreateBunkerInviteReply?>(null)
    val bunkerInvite: StateFlow<FfiCreateBunkerInviteReply?> = _bunkerInvite

    // Zap signers (the NIP-57 trust root, monetization.md § Zap receipts —
    // the trust model). Unlike the bunker invite, gated on LINKED alone — NOT
    // custodial: designating who may speak for your money is orthogonal to
    // where your key lives. [zapSignerGate] is the `zaps` plane's answer
    // over the Dim-3 courtesy read (`featuresGatePossibility`, the shared
    // `fauna_client_features::gate_possibility`): blocked still loading until
    // the read answers, blocked load-failed when it threw, absent when the
    // nest has no `zaps` row or hides it (`docs/goal/ui/README.md` § Offer
    // only what is possible, rules 2 and 5).
    private val _zapSigners = MutableStateFlow<List<ZapSignerItem>>(emptyList())
    val zapSigners: StateFlow<List<ZapSignerItem>> = _zapSigners

    private val _zapSignerGate = MutableStateFlow(zapsGateOver(null))
    val zapSignerGate: StateFlow<Possibility> = _zapSignerGate

    /** Succession-aftermath npub confirm banner (nostr.md § Key succession
     *  and rotation, leg 3) — checked once per [refresh] (the page's
     *  nav-enter path, tui/linux reference), never a one-shot local flag: the
     *  successor may reach this page long after the ceremony, so visibility
     *  is gated purely on the predicate, dismissible, never a blocking modal. */
    private val _npubConfirmationOwed = MutableStateFlow(false)
    val npubConfirmationOwed: StateFlow<Boolean> = _npubConfirmationOwed

    private val _isLoading = MutableStateFlow(true)
    val isLoading: StateFlow<Boolean> = _isLoading

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error

    fun refresh() {
        viewModelScope.launch {
            _isLoading.value = true
            _error.value = null
            try {
                val list = api.listBridges()
                val nostr = list.find { it.id == NOSTR_BRIDGE_ID }
                _registered.value = nostr != null
                _bridge.value = nostr
                _follows.value = if (nostr?.linked == true) {
                    try {
                        api.listBridgeFollows(NOSTR_BRIDGE_ID)
                    } catch (e: Exception) {
                        ShellLog.w("NostrVM", "list follows failed: ${e.message}")
                        emptyList()
                    }
                } else {
                    emptyList()
                }
                _zapSigners.value = if (nostr?.linked == true) {
                    try {
                        api.nostrZapSignersList()
                    } catch (e: Exception) {
                        ShellLog.w("NostrVM", "list zap signers failed: ${e.message}")
                        emptyList()
                    }
                } else {
                    emptyList()
                }
                // The Dim-3 courtesy read for the add button, answered by the
                // shared gate (tui's `designate_gate`): unknown is not
                // possible, so a fetch that threw blocks with load-failed.
                if (nostr?.linked == true) {
                    _zapSignerGate.value = try {
                        zapsGateOver(api.fetchFeatures())
                    } catch (e: Exception) {
                        ShellLog.w("NostrVM", "features read failed: ${e.message}")
                        readPossibility(ReadState.FAILED)
                    }
                }
                // Succession-aftermath npub confirm (leg 3), checked once on
                // this nav-enter read — the same "extra leg" tui's
                // Op::Refresh module docs describe. Best-effort: any unhappy
                // answer already degrades to `false` inside the FFI call, so
                // no separate try/catch is owed here.
                _npubConfirmationOwed.value = if (nostr?.linked == true) {
                    api.npubConfirmationOwed()
                } else {
                    false
                }
            } catch (e: Exception) {
                _error.value = e.message
            }
            _isLoading.value = false
        }
    }

    /** "Yes, that's my npub" — writes the confirm timestamp via the shared
     *  `fauna.state.nostr-confirmation` write path, then re-checks (non-optimistic, like every
     *  other mutation on this page — the banner's disappearance is a fresh
     *  read, not an assumed outcome). */
    fun confirmNpub() {
        viewModelScope.launch {
            _error.value = null
            try {
                api.confirmNostrNpub(System.currentTimeMillis() / 1000)
                _npubConfirmationOwed.value = if (_bridge.value?.linked == true) {
                    api.npubConfirmationOwed()
                } else {
                    false
                }
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    /** `fauna.nostr.bunker.create_invite` — mint a pending connection; reveals
     *  the one-time `bunker://` connect string. */
    fun connectApp() {
        viewModelScope.launch {
            _error.value = null
            try {
                _bunkerInvite.value = api.nostrBunkerCreateInvite()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    /** `fauna.nostr.zap_signers.add` — designate a signer. Client-glue 64-hex
     *  validation mirrors [addFollow]'s trim-and-guard shape: the nest
     *  refuses a non-64-hex key anyway, so this only spares a guaranteed
     *  round trip. Idempotent — a re-add REFRESHES the label, so re-entering
     *  a designated key is the only rename path there is. */
    fun addZapSigner(pubkey: String, label: String) {
        // Rule 6: the answer the button was painted from, asked again — the
        // plane's gate first, then the draft.
        answerRefusal(appContext, zapSignerAddAnswer(pubkey))?.let {
            _error.value = it.reason
            return
        }
        val trimmed = pubkey.trim()
        viewModelScope.launch {
            _error.value = null
            try {
                api.nostrZapSignersAdd(trimmed, label.trim())
                _zapSigners.value = api.nostrZapSignersList()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    /** `nostr-zap-signer-add-btn`'s whole answer (tui's `zap_signer_add_answer`):
     *  the `zaps` plane's gate ANDed with the shared draft predicate. */
    fun zapSignerAddAnswer(pubkey: String): Possibility =
        _zapSignerGate.value and askForm { nostrZapSignerAddPossibility(pubkey) }

    /** `fauna.nostr.zap_signers.remove` — stop trusting a signer, keyed by
     *  the row's own STORED pubkey. */
    fun removeZapSigner(pubkey: String) {
        viewModelScope.launch {
            _error.value = null
            try {
                api.nostrZapSignersRemove(pubkey)
                _zapSigners.value = api.nostrZapSignersList()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    fun link(mode: String, fields: Map<String, String>) {
        refusalOf(appContext) {
            nostrLinkPossibility(mode, fields["nsec"].orEmpty(), fields["bunker_url"].orEmpty())
        }?.let {
            _error.value = it
            return
        }
        viewModelScope.launch {
            _error.value = null
            try {
                api.linkBridge(NOSTR_BRIDGE_ID, mode, fields)
                // A fresh (re-)link is itself the new-npub remedy
                // (nostr.md:75): the owner just chose this key, so
                // best-effort record the confirmation — never blocking the
                // link on it (mirrors tui's Op::Link / linux's link-success
                // arm). Harmless on an account with no succession history:
                // the predicate short-circuits before ever reading it.
                try {
                    api.confirmNostrNpub(System.currentTimeMillis() / 1000)
                } catch (e: Exception) {
                    ShellLog.w("NostrVM", "best-effort npub confirm on link failed: ${e.message}")
                }
                refresh()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    /** Plain destructive action — no confirm overlay, matching the FaunaKit
     * `NostrSettingsView` (`Button(role: .destructive)`) and the other apps. */
    fun unlink() {
        viewModelScope.launch {
            _error.value = null
            try {
                api.unlinkBridge(NOSTR_BRIDGE_ID)
                refresh()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    /** A single-key partial update — never clobbers the other settings
     * (`fauna.bridges.set_settings` merges by key nest-side). */
    fun updateSetting(key: String, value: FfiCborValue) {
        viewModelScope.launch {
            _error.value = null
            try {
                api.updateBridgeSettings(NOSTR_BRIDGE_ID, FfiCborValue.Map(listOf(FfiCborEntry(key, value))))
                refresh()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    /** Read-modify-write the `relay_list` JSON-array setting (nostr.md § User
     * actions) — the nest republishes NIP-65 on change; the relay clients
     * themselves are shared Rust. */
    fun addRelay(url: String) {
        val current = parseRelayList(_bridge.value?.settings.orEmpty())
        // Blank, malformed, private-network and duplicate drafts are each
        // refused with their reason — a duplicate no longer drops silently.
        refusalOf(appContext) { nostrAddRelayPossibility(current, url) }?.let {
            _error.value = it
            return
        }
        val trimmed = trimmedRelayInput(url) ?: return
        val next = relayListAppending(current, trimmed) ?: return
        updateSetting("relay_list", FfiCborValue.Text(encodeRelayList(next)))
    }

    fun removeRelay(url: String) {
        val current = parseRelayList(_bridge.value?.settings.orEmpty())
        updateSetting("relay_list", FfiCborValue.Text(encodeRelayList(current - url)))
    }

    fun addFollow(pubkey: String, petname: String?) {
        refusalOf(appContext) { nostrAddFollowPossibility(pubkey) }?.let {
            _error.value = it
            return
        }
        val trimmed = pubkey.trim()
        viewModelScope.launch {
            _error.value = null
            try {
                // Queued with no nest: no list changes (tui's rule).
                val queued =
                    api.addBridgeFollowOrQueue(NOSTR_BRIDGE_ID, trimmed, petname?.trim()?.ifBlank { null })
                if (!queued) refresh()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    fun removeFollow(followId: String) {
        viewModelScope.launch {
            _error.value = null
            try {
                if (!api.removeBridgeFollowOrQueue(NOSTR_BRIDGE_ID, followId)) refresh()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }
}
