package com.fauna.app.core

import com.fauna.ffi.FfiLandedSuccession
import com.fauna.ffi.FfiSweepView
import org.json.JSONObject
import org.json.JSONTokener

/**
 * What an identity succession hands **across its own account switch** — the
 * ceremony's closing act (`identity-succession.md` § The RecoveryKey → *At
 * succession*). android's twin of apple's `SuccessionHandoff` and windows'
 * `FaunaApp.Core/Services/SuccessionHandoff.cs`.
 *
 * ── Why this is not in [ActorScope.dropActorScopedState] ────────────────────
 *
 * Every other piece of actor-scoped state is dropped at a switch precisely so
 * one identity's state cannot paint under the next one's. These are the
 * **inverse**: they belong to the *outgoing* identity's ceremony, and the
 * teardown they have to survive **is that ceremony's own closing act**. The
 * sweep is its result (rendered after the switch); the owed kit is its last
 * step, which only the successor's session can perform (the mint authenticates
 * as an identity that does not exist as a session until the teardown
 * completes); the predecessor id is what that mint must seal. tui states the
 * same rule at its own declaration sites (`apps/fauna-tui/src/app.rs`'s
 * `succession_*` fields). **Do not "fix" this by calling it from a switch
 * teardown.**
 *
 * ── What DOES clear it ───────────────────────────────────────────────────────
 *
 * A factory reset, via [clearOnFactoryReset]. A reset destroys every identity
 * on the box, so there is no successor left to owe a kit to and nothing left
 * for the sweep to describe. Same clear point as tui's and apple's.
 *
 * A process-wide `object` rather than a field on a view model deliberately:
 * this state's whole contract is that it outlives the objects a switch
 * rebuilds (the Account page's view models die with the back-stack entry the
 * switch tears down). Read and written on the main thread only.
 */
object SuccessionHandoff {
    /**
     * The successor owes itself a fresh kit — set the instant a succession
     * lands, **including on the persist-failure path**: the account moved
     * either way, so it is kitless and escrowless until this is discharged
     * (`RecoveryKitVM.dischargeOwedSuccessionKit`, which mints unbidden and
     * shows it).
     */
    var kitOwed = false
        private set

    /** The identity the account just moved *away* from, 64-hex. */
    var predecessorActorIdHex: String? = null
        private set

    /**
     * The identity that owes itself the kit — who [claimOwedKit] hands it to,
     * and nobody else.
     *
     * ⚠ **Not bookkeeping — the guard that makes the obligation survivable.**
     * The ceremony runs from a mounted Recovery kit section, and that section
     * outlives the ceremony by the width of the teardown: an unbound claim
     * would be taken by the OUTGOING session, which then mints against a nest
     * that has just revoked its bearers, fails, and leaves the flag spent
     * (measured on apple's first `--app macos` journey run, 2026-08-22).
     */
    var successorActorIdHex: String? = null
        private set

    /** Unix seconds the nest applied the succession, when the submit reply
     *  carried it. `null` on the reconcile arm is real, never a placeholder. */
    var succeededAtUnix: Long? = null
        private set

    /**
     * The pre-switch group sweep's own account of itself, in the e2e state
     * provider's vocabulary — `SweepStatus::state_json`, republished verbatim
     * as `data.succession_sweep` ([sweepStateForSerialization]).
     *
     * ⚠ Not a painting surface: its vocabulary (`no_engine`) deliberately
     * differs from [FfiSweepView.kind]'s (`no-engine`), which is the human one.
     */
    var sweepStateJson: String? = null
        private set

    /**
     * The pre-switch sweep as a surface paints it (`settings.md` § Recovery
     * kit → *The sweep's own lines*) — the human-facing twin of
     * [sweepStateJson], carried and cleared together. Handed to the shared
     * `sweepCopy` at paint time, never matched on `kind` here.
     */
    var sweep: FfiSweepView? = null
        private set

    /**
     * The successor a **relaunch adoption** owes the group sweep to — `null`
     * when nothing is owed (`succession-propagation.md` § Propagation → *Own
     * device fleet*, the relaunch-adoption clause). The ceremony runs its own
     * sweep before its switch; an adoption cannot (a refused launch never opens
     * the retired identity's engine), so it owes one, discharged by the
     * successor's first Recovery kit hydrate as an unbidden press of the retry.
     *
     * ⚠ Its own binding, deliberately not [successorActorIdHex]: [claimOwedKit]
     * clears that one, and a sweep deferred past the kit's claim would then be
     * owed to nobody.
     */
    var sweepOwedTo: String? = null
        private set

    /** The kit a section minted and could not show, bound to its successor. */
    private var strandedKit: Pair<String, StrandedKit>? = null

    /**
     * Record a landed succession, **before** the account switch that follows.
     * Called on both arms of `persisted`: the succession landed either way.
     */
    fun record(landed: FfiLandedSuccession, predecessorActorIdHex: String?) {
        kitOwed = true
        this.predecessorActorIdHex = predecessorActorIdHex
        successorActorIdHex = landed.newActorIdHex
        succeededAtUnix = landed.succeededAt
        sweepStateJson = landed.sweepStateJson
        sweep = landed.sweep
    }

    /**
     * Record a **relaunch adoption** — a launch refused as superseded whose
     * chain-verified successor this device held — **before** the account
     * switch that follows. The ceremony's closing obligations, minus what only
     * the ceremony had: the kit is owed and so is the group sweep; no sweep
     * report is carried (none ran) and no stamp (the reply that carried it was
     * the one lost). tui's `App::adopt_held_successor` is the twin.
     */
    fun recordRelaunchAdoption(predecessorActorIdHex: String, successorActorIdHex: String) {
        kitOwed = true
        this.predecessorActorIdHex = predecessorActorIdHex
        this.successorActorIdHex = successorActorIdHex
        succeededAtUnix = null
        sweepOwedTo = successorActorIdHex
    }

    /** Whether [actorIdHex] is the successor owed a kit — the post-auth
     *  navigation's peek; it claims nothing. */
    fun owesKitTo(actorIdHex: String?): Boolean =
        kitOwed && actorIdHex != null && successorActorIdHex == actorIdHex

    /**
     * Claim the owed kit for the session that is actually the successor — once.
     * **The actor bind** keeps the departing session from taking an obligation
     * it cannot perform; **the single claim** keeps two hydrates from minting
     * two kits, the second of which would register a kit nobody was shown.
     */
    fun claimOwedKit(asSuccessor: String): Boolean {
        if (!kitOwed || successorActorIdHex != asSuccessor) return false
        kitOwed = false
        predecessorActorIdHex = null
        successorActorIdHex = null
        return true
    }

    /**
     * Put a claimed-but-never-SHOWN obligation back, so the next live section
     * mints again — "minted" and "shown" are different events, and only the
     * second discharges anything.
     *
     * ⚠ **A second mint does NOT supersede a stranded one — it collides with
     * it**: the discharge mints with no prior kit, and the shared ceremony
     * refuses a no-prior mint over a registered chain head (`PriorKitRequired`).
     * So a section that minted off-screen hands the kit ITSELF over
     * ([stranded]), and the next live section shows it instead of minting
     * (apple measured ~280 refused mints before this, 2026-09-24). The stranded
     * kit lives in process memory only, never written anywhere
     * (`identity-succession.md` § The RecoveryKey → *Custody*).
     *
     * Re-binds to the successor it is given, so a re-arm can never hand the
     * obligation to a different identity.
     */
    fun rearmUnshownKit(successor: String, stranded: StrandedKit? = null) {
        kitOwed = true
        successorActorIdHex = successor
        strandedKit = stranded?.let { successor to it }
    }

    /** Take the stranded kit — once, and only for the successor it was minted
     *  for. Called right after a successful [claimOwedKit]. */
    fun takeStrandedKit(asSuccessor: String): StrandedKit? {
        val held = strandedKit ?: return null
        if (held.first != asSuccessor) return null
        strandedKit = null
        return held.second
    }

    /** Claim the owed sweep for the session that really is the successor —
     *  once; the same two guards as [claimOwedKit]. */
    fun claimOwedSweep(asSuccessor: String): Boolean {
        if (sweepOwedTo != asSuccessor) return false
        sweepOwedTo = null
        return true
    }

    /** Put a claimed sweep back when its press could not run at all. */
    fun rearmOwedSweep(successor: String) {
        sweepOwedTo = successor
    }

    /**
     * Replace the carried sweep after `recovery-kit-sweep-retry-button` (or an
     * owed sweep's discharge) — both halves together, so the human-facing view
     * and `data.succession_sweep` can never disagree about which pass is current.
     */
    fun replaceSweep(fresh: FfiSweepView, stateJson: String) {
        sweep = fresh
        sweepStateJson = stateJson
    }

    /** The reset clear point — see the type's doc for why it is the only one. */
    fun clearOnFactoryReset() {
        kitOwed = false
        predecessorActorIdHex = null
        successorActorIdHex = null
        succeededAtUnix = null
        sweepStateJson = null
        sweep = null
        sweepOwedTo = null
        strandedKit = null
    }

    /**
     * The sweep report as the state serializer publishes it: the decoded
     * object, or [JSONObject.NULL] when no succession ran on this app run —
     * absent stays null rather than an empty object, so a journey can tell "no
     * succession ran" from "one ran and swept nothing".
     */
    fun sweepStateForSerialization(): Any =
        sweepStateJson
            ?.let { runCatching { JSONTokener(it).nextValue() }.getOrNull() }
            ?: JSONObject.NULL

    /** A minted successor kit that no screen showed — carried to the next live
     *  section by [rearmUnshownKit]. */
    data class StrandedKit(
        val secretHex: String,
        val kitUri: String?,
        val escrowStored: Boolean,
        val landsAt: Long?,
    ) {
        // A logged value must not carry the secret or the URI that embeds it.
        override fun toString(): String =
            "StrandedKit(${secretHex.length} hex, uri=${kitUri != null}, escrow=$escrowStored, landsAt=$landsAt)"
    }
}
