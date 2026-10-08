package com.fauna.app.ui.viewmodel

import android.content.Context
import android.util.Log
import androidx.annotation.StringRes
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ShellLog
import com.fauna.app.core.StolenCeremonyHold
import com.fauna.app.core.SuccessionHandoff
import com.fauna.app.ui.util.getStringFmt
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiConnectionState
import com.fauna.ffi.FfiMintedKit
import com.fauna.ffi.FfiRecoveryKitStatus
import com.fauna.ffi.FfiStolenOutcome
import com.fauna.ffi.FfiSweepView
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.delay
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
 *   inline); feeds replace, stolen, veto and the escrow re-seal.
 * @property stolenConfirmInput the type-to-confirm buffer behind
 *   `identity-stolen-confirm-field`.
 * @property mintedSecretHex a secret a ceremony just minted, shown ONCE and
 *   dropped on leaving the page — there is no path that shows it again
 *   (`identity-succession.md` § The RecoveryKey — *Custody*). Also the
 *   successor's seed on the stolen ceremony's persist-failure arm.
 * @property mintedKitUri the `fauna://recovery` URI for [mintedSecretHex] — what
 *   the copy button and the QR carry; `null` when the builder refused, and on
 *   the persist-failure arm (a seed has no kit URI); the view then falls back to
 *   the bare secret.
 * @property mintedEscrowStored whether the escrow blob landed with the mint. ⚠
 *   `false` is NOT an error: the registration landed, so the shown secret is
 *   live and the only copy in existence.
 * @property sweepView the post-succession sweep's own view, mirrored from
 *   [SuccessionHandoff.sweep] at hydrate and replaced by a retry that swept —
 *   handed to the shared `sweepCopy` at paint time, never matched on `kind`.
 * @property busy a ceremony is in flight — every button reads it, so a second
 *   tap cannot start a second irreversible ceremony.
 */
data class RecoveryKitUiState(
    val status: FfiRecoveryKitStatus? = null,
    val phraseInput: String = "",
    val stolenConfirmInput: String = "",
    val mintedSecretHex: String? = null,
    val mintedKitUri: String? = null,
    val mintedEscrowStored: Boolean = true,
    val mintedLandsAt: Long? = null,
    val sweepView: FfiSweepView? = null,
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

    /**
     * Whether the succession trigger and its type-to-confirm gate render — the
     * stolen action belongs to "stolen (any)", every state. ⚠ Deliberately also
     * true while the status is UNREAD or the chain read failed (apple's
     * `stolenVisible`, windows' `StolenVisible`): this ceremony exists for an
     * owner a thief has locked out, the authenticated chain read is exactly
     * what fails for such an owner, and the ceremony's authorization is the
     * KIT, never the status. Only a status that POSITIVELY disallows hides it.
     */
    val stolenVisible: Boolean
        get() = status?.allowsStolen != false

    /** Whether `identity-stolen-button` is armed — a SECOND condition beside
     *  [stolenVisible], since `allowsStolen` is unconditionally true: this gate
     *  is the only thing between a stray tap and a re-pointed account. */
    val stolenArmed: Boolean
        get() = stolenConfirmInput == RecoveryKitVM.STOLEN_CONFIRM_WORD && !busy

    // `toString` never carries the secret, the URI that embeds it, or the
    // typed phrase — a logged state must not leak a root secret.
    override fun toString(): String =
        "RecoveryKitUiState(status=${status?.kind}, phrase=${phraseInput.length} chars, " +
            "confirm=${stolenConfirmInput.length} chars, " +
            "minted=${mintedSecretHex?.length ?: 0} hex, escrowStored=$mintedEscrowStored, " +
            "landsAt=$mintedLandsAt, sweep=${sweepView?.kind}, busy=$busy)"
}

/**
 * The Recovery kit section's state holder — android's port of apple's
 * `RecoveryKitVM` / windows' `RecoveryKitViewModel`.
 *
 * **It decides nothing.** The status line's state, which actions each state
 * enables, what the succession ceremony did and which sweep lines say what
 * arrive already decided from `libs/fauna-ffi/src/recovery.rs`. Never derive
 * enablement from `status.kind` here: `allowsStolen` is unconditionally true,
 * and `allowsReplace` stays true during a pending window (pinned Rust-side by
 * `enablement_does_not_follow_from_the_status_kind`).
 *
 * The kit-in-hand and confirm checks are re-made in each handler, not only in
 * the render: a disabled control emits no gesture, but a test agent driving the
 * id reaches the handler, and a ceremony must refuse out loud rather than run
 * on nothing.
 */
@HiltViewModel
class RecoveryKitVM @Inject constructor(
    private val api: ApiClient,
    @ApplicationContext private val context: Context,
    /** Where the stolen ceremony reports that it owns the Account page, so a
     *  supersession it causes is held back rather than tearing the page down. */
    val ceremonyHold: StolenCeremonyHold,
) : ViewModel() {

    private val _state = MutableStateFlow(RecoveryKitUiState())
    val state: StateFlow<RecoveryKitUiState> = _state.asStateFlow()

    /**
     * The section's one error sentence, routed by the screen onto the page's
     * `error-message` banner and then [consumeError]ed so an identical second
     * failure still shows. ⚠ Never assign it directly outside
     * [parkStolenMessage] — every other writer goes through [setError], which
     * drops the write while a parked message is pending.
     */
    val errorMessage = MutableStateFlow<String?>(null)

    /**
     * True while a stolen-ceremony parked message — the landed arm's
     * persist-failure message, or the undecided outcome that
     * `carriesTheOnlySeed` — is on the Account page, not yet acknowledged
     * (`settings.md` § Recovery kit → *The persist-failure message survives the
     * page*). It is the ONLY surviving copy of the account's new key, so it wins
     * over every other write until the user leaves the Account page or the
     * signed-in identity changes ([acknowledgeStolenFailedMessage]).
     */
    var stolenFailedMessagePending = false
        private set

    /**
     * Whether this view model's section is composed. ⚠ Load-bearing: the
     * succession's owed kit is a ONE-SHOT claim, and a section on its way out
     * that claimed it would mint the successor's only RecoveryKey into a view
     * the user can no longer see (apple's `onScreen`, measured on iOS
     * 2026-08-26). Defaults to `true`, so a hydrate that outruns the section's
     * enter edge is never wrongly blocked — only an observed leave clears it.
     */
    var onScreen = true

    /** The identity [state] belongs to — compared, never rendered. */
    private var hydratedActor: String? = null

    /** The client's transport state — the section re-keys its hydrate on the
     *  seat it yields ([boundSeat]). */
    val connectionState: StateFlow<FfiConnectionState> get() = api.connectionState

    /** The actor the client's seat signs as, or `null` with none. */
    fun boundSeat(): String? = api.boundActorIdHex

    /**
     * Read the chain for [sessionActorIdHex], then discharge whatever a
     * succession owes this session — the owed sweep first, then the owed kit
     * (the ceremony's own order: sweep, then kit).
     *
     * A different identity than the one the state was read for drops everything
     * first: the previous account's status is not a weaker answer for this one,
     * it is the WRONG one — every action's enablement hangs off it — and a kit
     * minted for the account just left must not linger on screen for this one.
     *
     * The navigation that lands the successor here is the shell's own post-auth
     * act (`identity-succession.md` § The RecoveryKey → *At succession*:
     * navigate synchronously, mint after) — never this view model's.
     */
    fun hydrate(sessionActorIdHex: String?) {
        val actor = sessionActorIdHex ?: "-"
        ShellLog.i(
            TAG,
            "[succession-kit] hydrate: actor=$actor was=${hydratedActor ?: "<first>"} " +
                "kitOwed=${SuccessionHandoff.kitOwed} owedTo=${SuccessionHandoff.successorActorIdHex ?: "-"}",
        )
        if (hydratedActor != null && hydratedActor != actor) resetForIdentityChange()
        hydratedActor = actor
        // Mirrored, not consumed: the hand-off's sweep outlives every hydrate,
        // and only `record` or a swept retry ever changes it.
        _state.update { it.copy(sweepView = SuccessionHandoff.sweep) }
        viewModelScope.launch {
            loadStatus()
            if (onScreen) {
                dischargeOwedSweep(sessionActorIdHex)
                dischargeOwedSuccessionKit(sessionActorIdHex)
                // The residual race, closed: the section can leave between the
                // check above and the mint landing. A kit held by a section no
                // longer on screen was shown to nobody — hand the kit ITSELF to
                // the next live section (it is now the registered chain head, so
                // a second no-prior mint would be refused).
                val held = heldKit
                if (!onScreen && held != null && sessionActorIdHex != null) {
                    ShellLog.w(TAG, "[succession-kit] minted into a section that left the screen — handing the kit to the next live one")
                    SuccessionHandoff.rearmUnshownKit(sessionActorIdHex, held)
                    clearHeldSecrets()
                }
            } else if (SuccessionHandoff.kitOwed) {
                ShellLog.i(TAG, "[succession-kit] discharge skipped: section is off-screen — leaving the obligation for the live one")
            }
        }
    }

    fun onPhraseChange(value: String) {
        _state.update { it.copy(phraseInput = value) }
    }

    fun onStolenConfirmChange(value: String) {
        _state.update { it.copy(stolenConfirmInput = value) }
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
            setError(failed(R.string.settings_recovery_kit_status_failed, e))
        }
    }

    /** `recovery-kit-create-button` (no phrase) and `recovery-kit-replace-button`
     *  (the phrase in hand) — one ceremony, two authorization arms. */
    fun createOrReplaceKit(usingHeldPhrase: Boolean) {
        viewModelScope.launch { createOrReplaceKitNow(usingHeldPhrase) }
    }

    /** `recovery-kit-lost-button` — opens the 30-day window rather than taking
     *  effect now, yet still mints and shows a secret immediately. */
    fun requestSeedAloneReplacement() {
        if (_state.value.busy) return
        viewModelScope.launch { runMint { api.recoveryRequestSeedAloneReplacement() } }
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
     * `identity-stolen-button` — the irreversible succession ceremony.
     *
     * Both gates are re-checked HERE: the `SUCCEED` literal first (an armed
     * render is not proof — a test agent driving the id reaches this handler),
     * then the kit in hand.
     *
     * On the persisted landed arm the account belongs to a new identity and this
     * session's bearers were revoked inside the nest's own transaction, so
     * [onSucceeded] switches to the successor. ⚠ On every other arm it is NOT
     * called — above all the persist-failure arm, where tearing the session down
     * takes the only copy of the successor's seed with it.
     *
     * @param predecessorActorIdHex the signed-in identity, read BEFORE the
     *   switch makes it unnameable — what the owed kit's mint must seal.
     */
    fun succeedWithHeldKit(predecessorActorIdHex: String?, onSucceeded: (String) -> Unit) {
        val s = _state.value
        if (s.busy) return
        if (s.stolenConfirmInput != STOLEN_CONFIRM_WORD) {
            setError(context.getString(R.string.settings_recovery_kit_stolen_confirm_placeholder))
            return
        }
        if (s.phraseInput.isEmpty()) {
            setError(context.getString(R.string.settings_recovery_kit_kit_phrase_required))
            return
        }
        _state.update { it.copy(busy = true) }
        // From here the nest may commit at any moment, and this session's
        // reconnect is then refused as superseded — held back until the result
        // below is handled.
        ceremonyHold.ceremonyStarted()
        viewModelScope.launch {
            var adopted = false
            try {
                val outcome = api.successionSucceedWithHeldKit(s.phraseInput)
                adopted = applyStolenOutcome(outcome, predecessorActorIdHex, onSucceeded)
            } catch (e: Exception) {
                // Only a failure BEFORE the ceremony could start reaches here
                // (no connection, unparseable secret bytes) — every ceremony that
                // ran is an outcome above, never an error.
                setError(failed(R.string.settings_recovery_kit_stolen_ceremony_failed, e))
            } finally {
                _state.update { it.copy(busy = false) }
            }
            ceremonyHold.ceremonyEnded(adopted = adopted, messageParked = stolenFailedMessagePending)
        }
    }

    /**
     * Fold the ceremony's typed outcome onto the section (`settings.md`
     * § Recovery kit → *The ceremony's outcome is headlined by its arm*).
     * Returns whether the device switched to the successor.
     *
     * Landed keeps its two halves (the switch, or the parked persist-failure
     * message). Every other arm paints the shared sentence verbatim and wraps
     * nothing; the one carrying the only copy of the successor seed is parked
     * exactly as the persist-failure message is — decided by the record's flag,
     * never by reading its kind or key. apple's `applyStolenOutcome`.
     */
    internal fun applyStolenOutcome(
        outcome: FfiStolenOutcome,
        predecessorActorIdHex: String?,
        onSucceeded: (String) -> Unit,
    ): Boolean {
        val landed = outcome.landed
        if (landed == null) {
            val sentence = resolveLocalized(context, outcome.message) ?: outcome.kind
            if (outcome.carriesTheOnlySeed) parkStolenMessage(sentence) else setError(sentence)
            return false
        }
        _state.update { it.copy(phraseInput = "", stolenConfirmInput = "") }
        // Hand the ceremony's survivors over BEFORE the switch below: it tears
        // this session (and this view model) down moments from now, and
        // everything recorded here is declared to outlive that.
        SuccessionHandoff.record(landed, predecessorActorIdHex)
        if (landed.persisted) {
            onSucceeded(landed.newActorIdHex)
            return true
        }
        // The one landed arm where the secret must go on screen. Never phrased
        // as "nothing happened" — the account DID move. A seed, not a kit: it
        // has no `fauna://recovery` form, and an earlier mint's URI must not
        // ride along on it.
        _state.update {
            it.copy(
                mintedSecretHex = landed.successorSecretHex,
                mintedKitUri = null,
                mintedEscrowStored = true,
                mintedLandsAt = null,
            )
        }
        parkStolenMessage(
            context.getStringFmt(R.string.settings_recovery_kit_stolen_persist_failed, landed.successorSecretHex),
        )
        return false
    }

    /**
     * `recovery-kit-sweep-retry-button` — finish a sweep the ceremony left
     * unfinished (`settings.md` § Recovery kit → *Finishing an unfinished group
     * sweep*). **The answer is the product here:** the button renders on
     * unfinished work, deliberately NOT on whether this device can retry, so a
     * press that cannot sweep must still say something. Only the `swept` arm
     * says nothing on `error-message` — its outcome renders through the sweep's
     * own lines, off the FRESH view this replaces [RecoveryKitUiState.sweepView]
     * with (and `data.succession_sweep` with it).
     */
    fun retrySweep() {
        if (_state.value.busy) return
        _state.update { it.copy(busy = true) }
        viewModelScope.launch {
            try {
                val answer = api.successionRetryGroupSweep()
                val fresh = answer.sweep
                val json = answer.sweepStateJson
                if (fresh != null && json != null) {
                    _state.update { it.copy(sweepView = fresh) }
                    SuccessionHandoff.replaceSweep(fresh, json)
                }
                setError(resolveLocalized(context, answer.message))
            } catch (e: Exception) {
                setError(failed(R.string.settings_recovery_kit_action_failed, e))
            } finally {
                _state.update { it.copy(busy = false) }
            }
        }
    }

    /**
     * Discharge the group sweep a **relaunch adoption** owes — an unbidden
     * press of `recovery-kit-sweep-retry-button`, run BEFORE the kit's mint
     * (`succession-propagation.md` § Propagation → *Own device fleet*, the
     * relaunch-adoption clause). Nothing happens unless
     * [SuccessionHandoff.sweepOwedTo] names this session. The answer folds
     * where a press would: its sentence (if any) on `error-message`, and a
     * report parked in the sweep view and `data.succession_sweep` — shared Rust
     * picks which.
     *
     * A busy view model, or a seat that is not yet the successor's, leaves the
     * obligation standing for the next hydrate rather than spending it.
     */
    internal suspend fun dischargeOwedSweep(sessionActorIdHex: String?) {
        if (sessionActorIdHex == null || SuccessionHandoff.sweepOwedTo != sessionActorIdHex) return
        if (_state.value.busy || api.boundActorIdHex != sessionActorIdHex) {
            ShellLog.i(TAG, "[succession-sweep] discharge deferred (busy=${_state.value.busy}) — obligation kept for the next pass")
            return
        }
        if (!SuccessionHandoff.claimOwedSweep(sessionActorIdHex)) return
        _state.update { it.copy(busy = true) }
        try {
            val owed = api.successionDischargeOwedSweep()
            _state.update { it.copy(sweepView = owed.parked) }
            SuccessionHandoff.replaceSweep(owed.parked, owed.parkedStateJson)
            ShellLog.i(TAG, "[succession-sweep] owed sweep answered ${owed.answer.kind}")
            setError(resolveLocalized(context, owed.answer.message))
        } catch (e: Exception) {
            // Only a session that cannot reach the nest throws, before anything
            // ran — put the obligation back rather than spend it.
            SuccessionHandoff.rearmOwedSweep(sessionActorIdHex)
            setError(failed(R.string.settings_recovery_kit_action_failed, e))
        } finally {
            _state.update { it.copy(busy = false) }
        }
    }

    /**
     * Mint and show the successor's recovery kit — the **closing act** of an
     * identity succession, run on the successor's own session
     * (`identity-succession.md` § The RecoveryKey → *At succession*). Nothing
     * happens unless [SuccessionHandoff] owes THIS session a kit, so an ordinary
     * visit pays one comparison.
     *
     * The guards come BEFORE the one-shot claim: a busy view model or a seat
     * still signing as someone else (the predecessor's revoked socket) defer
     * with the flag kept, so a later hydrate — the section re-keys on the seat —
     * performs it. After the claim, every mint failure would spend the
     * obligation having shown nothing, and a first-attempt failure is the
     * NORMAL case (the ceremony revokes every session, so the successor's first
     * mint races its own reconnect) — so it retries on a bounded backoff and
     * re-arms rather than swallow it. Except when the chain already HAS a head
     * ([mintCanNeverLand]): no no-prior mint can ever land there, and re-arming
     * is what looped ~280 refused mints on apple (2026-09-24).
     */
    internal suspend fun dischargeOwedSuccessionKit(sessionActorIdHex: String?) {
        if (_state.value.busy) {
            ShellLog.i(TAG, "[succession-kit] discharge deferred (busy) — flag kept for the next pass")
            return
        }
        if (sessionActorIdHex == null) return
        val seat = api.boundActorIdHex
        if (seat != sessionActorIdHex) {
            if (SuccessionHandoff.owesKitTo(sessionActorIdHex)) {
                ShellLog.i(TAG, "[succession-kit] discharge deferred: seat is ${seat ?: "<no-secret>"} but the owed successor is $sessionActorIdHex — flag kept for the next pass")
            }
            return
        }
        if (!SuccessionHandoff.claimOwedKit(sessionActorIdHex)) return
        // A kit an off-screen section already minted is the one to SHOW — it is
        // the registered chain head, so a fresh no-prior mint would be refused.
        SuccessionHandoff.takeStrandedKit(sessionActorIdHex)?.let { stranded ->
            showKit(stranded)
            ShellLog.i(TAG, "[succession-kit] discharge claimed by $sessionActorIdHex → showing the kit an off-screen section minted")
            return
        }
        ShellLog.i(TAG, "[succession-kit] discharge claimed by $sessionActorIdHex → minting")
        for (attempt in 1..SUCCESSION_MINT_ATTEMPTS) {
            createOrReplaceKitNow(usingHeldPhrase = false)
            if (_state.value.mintedSecretHex != null) return
            loadStatus()
            if (mintCanNeverLand(_state.value.status)) {
                ShellLog.e(TAG, "[succession-kit] a kit is already registered for $sessionActorIdHex and no screen holds it — not re-arming a mint the chain head refuses")
                return
            }
            ShellLog.w(TAG, "[succession-kit] mint attempt $attempt of $SUCCESSION_MINT_ATTEMPTS showed nothing")
            if (attempt < SUCCESSION_MINT_ATTEMPTS) delay(successionMintBackoffMs)
        }
        ShellLog.e(TAG, "[succession-kit] every mint attempt failed — re-arming the obligation for $sessionActorIdHex rather than spending it")
        SuccessionHandoff.rearmUnshownKit(sessionActorIdHex)
    }

    /** The backoff between the discharge's mint attempts — a `var` so a unit
     *  test runs the bounded retry without sleeping. */
    internal var successionMintBackoffMs = 3_000L

    /** The kit this view model holds on screen, packaged for
     *  [SuccessionHandoff.rearmUnshownKit] — `null` when nothing was minted. */
    val heldKit: SuccessionHandoff.StrandedKit?
        get() = _state.value.let { s ->
            s.mintedSecretHex?.let {
                SuccessionHandoff.StrandedKit(it, s.mintedKitUri, s.mintedEscrowStored, s.mintedLandsAt)
            }
        }

    /** Drop everything a **different identity** put here — held secrets, and
     *  the status those were read against (the load-bearing half). */
    private fun resetForIdentityChange() {
        clearHeldSecrets()
        _state.update { it.copy(status = null) }
        setError(null)
    }

    /**
     * Drop everything the screen was holding — called when the section leaves
     * composition. The minted secret must not survive the view that displayed
     * it, and the two typed buffers are a recovery phrase and a confirm token
     * (an armed gate must not survive a page leave). Leaving is also the parked
     * message's acknowledgment edge, so it is discharged here first.
     */
    fun clearHeldSecrets() {
        acknowledgeStolenFailedMessage()
        _state.update {
            it.copy(
                phraseInput = "",
                stolenConfirmInput = "",
                mintedSecretHex = null,
                mintedKitUri = null,
                mintedEscrowStored = true,
                mintedLandsAt = null,
            )
        }
    }

    /** Discharge a still-pending parked message: the user left the Account
     *  page or the signed-in identity changed — either way they had the whole
     *  prior visit to read or copy the key. */
    fun acknowledgeStolenFailedMessage() {
        stolenFailedMessagePending = false
    }

    /** Write the error sentence — UNLESS a parked stolen-ceremony message is
     *  pending, in which case the write is dropped rather than clobbering the
     *  only surviving copy of the successor's key. */
    private fun setError(text: String?) {
        if (stolenFailedMessagePending) return
        errorMessage.value = text
    }

    /** The park write itself — deliberately unguarded ([setError] would drop
     *  it, since this call is what MAKES the message pending). */
    private fun parkStolenMessage(sentence: String) {
        errorMessage.value = sentence
        stolenFailedMessagePending = true
    }

    /** Put a kit on screen — the display half of [runMint], shared with the
     *  stranded-kit hand-over so both paint the same four fields. */
    private fun showKit(kit: SuccessionHandoff.StrandedKit) {
        _state.update {
            it.copy(
                mintedSecretHex = kit.secretHex,
                mintedKitUri = kit.kitUri,
                mintedEscrowStored = kit.escrowStored,
                mintedLandsAt = kit.landsAt,
            )
        }
        setError(null)
    }

    private suspend fun createOrReplaceKitNow(usingHeldPhrase: Boolean) {
        val s = _state.value
        if (s.busy) return
        if (usingHeldPhrase && s.phraseInput.isEmpty()) {
            setError(context.getString(R.string.settings_recovery_kit_kit_phrase_required))
            return
        }
        val held = if (usingHeldPhrase) s.phraseInput else null
        runMint { api.recoveryCreateKit(held) }
    }

    private fun runWithHeldKit(@StringRes failure: Int, ceremony: suspend (String) -> Any) {
        val s = _state.value
        if (s.busy) return
        if (s.phraseInput.isEmpty()) {
            setError(context.getString(R.string.settings_recovery_kit_kit_phrase_required))
            return
        }
        _state.update { it.copy(busy = true) }
        viewModelScope.launch {
            try {
                ceremony(s.phraseInput)
                _state.update { it.copy(phraseInput = "") }
                loadStatus()
            } catch (e: Exception) {
                setError(failed(failure, e))
            } finally {
                _state.update { it.copy(busy = false) }
            }
        }
    }

    /** Run a kit-minting ceremony and surface its secret once. */
    private suspend fun runMint(ceremony: suspend () -> FfiMintedKit) {
        _state.update { it.copy(busy = true) }
        try {
            val kit = ceremony()
            // Display BEFORE anything else can fail: at this instant the
            // secret exists nowhere else in the world. The URI is built in the
            // same step (a LOCAL read), so no frame paints a copy button the URI
            // has not reached; a refusal costs the account params, never the
            // display.
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
            setError(null)
            loadStatus()
        } catch (e: Exception) {
            setError(failed(R.string.settings_recovery_kit_action_failed, e))
        } finally {
            _state.update { it.copy(busy = false) }
        }
    }

    private fun failed(@StringRes id: Int, e: Exception): String =
        context.getStringFmt(id, e.message?.takeIf { it.isNotBlank() } ?: e.javaClass.simpleName)

    companion object {
        private const val TAG = "RecoveryKitVM"

        /**
         * The type-to-confirm token. **Never localized** — only its prompt is
         * (`recovery_kit.stolen_confirm_placeholder`), or the gate would differ
         * per locale. Every app spells the same literal, exactly as account
         * deletion spells `"DELETE"`.
         */
        const val STOLEN_CONFIRM_WORD = "SUCCEED"

        /** How many times the succession discharge tries to mint before giving
         *  the obligation back — sized against the reconnect it races. */
        internal const val SUCCESSION_MINT_ATTEMPTS = 5

        /** Whether the discharge's no-prior mint is refused by construction on
         *  this chain read: the shared `allowsCreate` says a create needs no
         *  prior kit only when none is registered. An unread chain (`null`) is
         *  NOT that answer — the read raced the reconnect, so retrying is worth
         *  it. */
        internal fun mintCanNeverLand(status: FfiRecoveryKitStatus?): Boolean =
            status?.allowsCreate == false
    }
}
