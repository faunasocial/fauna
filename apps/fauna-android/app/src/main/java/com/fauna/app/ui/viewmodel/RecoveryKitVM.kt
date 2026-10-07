package com.fauna.app.ui.viewmodel

import android.content.Context
import android.util.Log
import androidx.annotation.StringRes
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.ui.util.getStringFmt
import com.fauna.ffi.FfiMintedKit
import com.fauna.ffi.FfiRecoveryKitStatus
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * Everything the Settings/Account **Recovery kit** section renders
 * (`docs/goal/ui/settings.md` § Recovery kit) — one plain value, so
 * [com.fauna.app.ui.components.RecoveryKitSectionContent] stays stateless and
 * renders under Robolectric with no VM and no FFI.
 *
 * @property status the chain read, or `null` until it resolves (or when it
 *   failed) — an un-hydrated section must not *claim* a state; the view paints
 *   `status_loading` in that window.
 * @property phraseInput the kit-in-hand buffer behind
 *   `recovery-entry-phrase-field` (the onboarding screen's own id, reused
 *   inline); feeds replace, veto and the escrow re-seal.
 * @property mintedSecretHex a secret a ceremony just minted, shown ONCE and
 *   dropped on leaving the page — there is no path that shows it again
 *   (`identity-succession.md` § The RecoveryKey — *Custody*).
 * @property mintedKitUri the `fauna://recovery` URI for [mintedSecretHex] — what
 *   the copy button and the QR carry; `null` when the builder refused, and the
 *   view then falls back to the bare secret (the restore parser takes both).
 * @property mintedEscrowStored whether the escrow blob landed with the mint. ⚠
 *   `false` is NOT an error: the registration landed, so the shown secret is
 *   live and the only copy in existence.
 * @property busy a ceremony is in flight — every button reads it, so a second
 *   tap cannot start a second irreversible ceremony.
 */
data class RecoveryKitUiState(
    val status: FfiRecoveryKitStatus? = null,
    val phraseInput: String = "",
    val mintedSecretHex: String? = null,
    val mintedKitUri: String? = null,
    val mintedEscrowStored: Boolean = true,
    val mintedLandsAt: Long? = null,
    val busy: Boolean = false,
) {
    /** The phrase field renders while a ceremony that consumes one is
     *  reachable — replace, stolen, veto or the no-escrow re-seal (tui's
     *  gate). Also with the status UNREAD: an unread status must not take the
     *  field away from a ceremony that needs it (apple's `phraseFieldVisible`). */
    val phraseFieldVisible: Boolean
        get() {
            val s = status ?: return true
            return s.allowsReplace || s.allowsStolen || s.allowsEscrowReseal ||
                s.pendingLandsAt != null
        }

    // `toString` never carries the secret, the URI that embeds it, or the
    // typed phrase — a logged state must not leak a root secret.
    override fun toString(): String =
        "RecoveryKitUiState(status=${status?.kind}, phrase=${phraseInput.length} chars, " +
            "minted=${mintedSecretHex?.length ?: 0} hex, escrowStored=$mintedEscrowStored, " +
            "landsAt=$mintedLandsAt, busy=$busy)"
}

/**
 * The Recovery kit section's state holder — android's port of apple's
 * `RecoveryKitVM` / windows' `RecoveryKitViewModel`.
 *
 * **It decides nothing.** The status line's state and which actions each state
 * enables arrive already decided from `libs/fauna-ffi/src/recovery.rs`
 * ([ApiClient.recoveryKitStatus]). Never derive enablement from `status.kind`
 * here: `allowsStolen` is unconditionally true, and `allowsReplace` stays true
 * during a pending window (pinned Rust-side by
 * `enablement_does_not_follow_from_the_status_kind`).
 *
 * The kit-in-hand checks are re-made in each handler, not only in the render:
 * a disabled control emits no gesture, but a test agent driving the id reaches
 * the handler, and a ceremony must refuse out loud rather than run on nothing.
 */
@HiltViewModel
class RecoveryKitVM @Inject constructor(
    private val api: ApiClient,
    @ApplicationContext private val context: Context,
) : ViewModel() {

    private val _state = MutableStateFlow(RecoveryKitUiState())
    val state: StateFlow<RecoveryKitUiState> = _state.asStateFlow()

    /** The section's one error sentence, routed by the screen onto the page's
     *  `error-message` banner and then [consumeError]ed so an identical
     *  second failure still shows. */
    val errorMessage = MutableStateFlow<String?>(null)

    /** The identity [state] belongs to — compared, never rendered. */
    private var hydratedActor: String? = null

    /**
     * Read the chain for [sessionActorIdHex]. A different identity than the one
     * the state was read for drops everything first: the previous account's
     * status is not a weaker answer for this one, it is the WRONG one — every
     * action's enablement hangs off it — and a kit minted for the account just
     * left must not linger on screen for this one.
     */
    fun hydrate(sessionActorIdHex: String?) {
        val actor = sessionActorIdHex ?: "-"
        if (hydratedActor != null && hydratedActor != actor) {
            clearHeldSecrets()
            _state.update { it.copy(status = null) }
        }
        hydratedActor = actor
        viewModelScope.launch { loadStatus() }
    }

    fun onPhraseChange(value: String) {
        _state.update { it.copy(phraseInput = value) }
    }

    fun consumeError() {
        errorMessage.value = null
    }

    /**
     * Read the section's state from the **registration chain**. A section that
     * cannot read its chain paints no state and says why — it must not fall
     * back to a state it did not observe.
     */
    suspend fun loadStatus() {
        try {
            val status = api.recoveryKitStatus()
            _state.update { it.copy(status = status) }
        } catch (e: Exception) {
            _state.update { it.copy(status = null) }
            errorMessage.value = failed(R.string.settings_recovery_kit_status_failed, e)
        }
    }

    /** `recovery-kit-create-button` (no phrase) and `recovery-kit-replace-button`
     *  (the phrase in hand) — one ceremony, two authorization arms. */
    fun createOrReplaceKit(usingHeldPhrase: Boolean) {
        val s = _state.value
        if (s.busy) return
        if (usingHeldPhrase && s.phraseInput.isEmpty()) {
            errorMessage.value = context.getString(R.string.settings_recovery_kit_kit_phrase_required)
            return
        }
        val held = if (usingHeldPhrase) s.phraseInput else null
        runMint { api.recoveryCreateKit(held) }
    }

    /** `recovery-kit-lost-button` — opens the 30-day window rather than taking
     *  effect now, yet still mints and shows a secret immediately. */
    fun requestSeedAloneReplacement() {
        if (_state.value.busy) return
        runMint { api.recoveryRequestSeedAloneReplacement() }
    }

    /** `recovery-pending-veto-button` — contest a pending replacement. */
    fun vetoPendingReplacement() = runWithHeldKit(R.string.settings_recovery_kit_veto_failed) { held ->
        api.recoveryVetoPendingReplacement(held)
    }

    /** `recovery-kit-escrow-reseal-button` — the no-escrow repair. Restores
     *  phrase recovery WITHOUT retiring the kit in hand, which is why it is not
     *  a create. */
    fun resealEscrowWithHeldKit() = runWithHeldKit(R.string.settings_recovery_kit_action_failed) { held ->
        api.recoveryResealEscrowWithHeldKit(held)
    }

    /**
     * Drop everything the screen was holding — called when the section leaves
     * composition. The minted secret must not survive the view that displayed
     * it, and the typed buffer is a recovery phrase.
     */
    fun clearHeldSecrets() {
        _state.update {
            it.copy(
                phraseInput = "",
                mintedSecretHex = null,
                mintedKitUri = null,
                mintedEscrowStored = true,
                mintedLandsAt = null,
            )
        }
    }

    private fun runWithHeldKit(@StringRes failure: Int, ceremony: suspend (String) -> Any) {
        val s = _state.value
        if (s.busy) return
        if (s.phraseInput.isEmpty()) {
            errorMessage.value = context.getString(R.string.settings_recovery_kit_kit_phrase_required)
            return
        }
        _state.update { it.copy(busy = true) }
        viewModelScope.launch {
            try {
                ceremony(s.phraseInput)
                _state.update { it.copy(phraseInput = "") }
                loadStatus()
            } catch (e: Exception) {
                errorMessage.value = failed(failure, e)
            } finally {
                _state.update { it.copy(busy = false) }
            }
        }
    }

    /** Run a kit-minting ceremony and surface its secret once. */
    private fun runMint(ceremony: suspend () -> FfiMintedKit) {
        _state.update { it.copy(busy = true) }
        viewModelScope.launch {
            try {
                val kit = ceremony()
                // Display BEFORE anything else can fail: at this instant the
                // secret exists nowhere else in the world. The URI is built in
                // the same step (a LOCAL read), so no frame paints a copy button
                // the URI has not reached; a refusal costs the account params,
                // never the display.
                val uri = try {
                    api.recoveryKitDisplayUri(kit.secretHex)
                } catch (e: Exception) {
                    Log.w(TAG, "[recovery-kit] display URI unavailable (${e.javaClass.simpleName}) — copy and QR carry the bare kit")
                    null
                }
                _state.update {
                    it.copy(
                        mintedSecretHex = kit.secretHex,
                        mintedKitUri = uri,
                        mintedEscrowStored = kit.escrowStored,
                        mintedLandsAt = kit.landsAt,
                        phraseInput = "",
                    )
                }
                loadStatus()
            } catch (e: Exception) {
                errorMessage.value = failed(R.string.settings_recovery_kit_action_failed, e)
            } finally {
                _state.update { it.copy(busy = false) }
            }
        }
    }

    private fun failed(@StringRes id: Int, e: Exception): String =
        context.getStringFmt(id, e.message?.takeIf { it.isNotBlank() } ?: e.javaClass.simpleName)

    private companion object {
        const val TAG = "RecoveryKitVM"
    }
}
