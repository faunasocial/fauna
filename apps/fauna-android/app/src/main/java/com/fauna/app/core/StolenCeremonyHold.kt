package com.fauna.app.core

import javax.inject.Inject
import javax.inject.Singleton

/**
 * What this device's stolen-identity ceremony holds while it owns the Account
 * page (`docs/goal/ui/settings.md` § Recovery kit → *The persist-failure
 * message survives the page*): the page's other error writers, and the
 * supersession escalation the ceremony itself causes. android's port of apple's
 * FaunaKit `StolenCeremonyHold`; tui's `App::defer_own_supersession` and its
 * `PageErrors::stolen_failed_pending` guard are the model.
 *
 * **The writers.** When the successor's seed could not be stored, the ceremony
 * parks the new key on Account's `error-message` (the shell's `AppMessages`
 * error banner) — the only copy in existence. Every other writer on that page
 * drops its error while [messagePending] is set ([admits]); clears still land,
 * since they cannot hide the key.
 *
 * **The escalation.** The ceremony is what supersedes the identity, so the
 * running session's reconnect supervisor reports `SUPERSEDED` the moment the
 * nest commits — typically before the ceremony's own result is handled.
 * Escalating then (drop the session, re-enter launch) would take the page, and
 * the ceremony's result, down with it. So while the ceremony runs, or its
 * parked message is pending, or its outcome is on screen, the escalation is
 * recorded rather than performed, and it runs once the user leaves Account —
 * or at once, when the ceremony ends while the user is elsewhere and nothing is
 * parked. Only the supersession verdict goes through here: a nest-identity
 * change or a refused sign-in is never the ceremony's own doing.
 *
 * One per process (Hilt `@Singleton`), because its three reporters live at
 * three different altitudes — the shell's session-ending collector
 * ([com.fauna.app.ui.viewmodel.AppLaunchVM.routeSessionEnding]), the ceremony
 * ([com.fauna.app.ui.viewmodel.RecoveryKitVM]) and the Account page's lifecycle
 * ([com.fauna.app.ui.components.RecoveryKitSection]) — and the section's view
 * model dies with the back-stack entry. A unit test constructs its own.
 * Main thread only.
 */
@Singleton
class StolenCeremonyHold @Inject constructor() {

    /** True from the stolen ceremony's dispatch until its result is handled. */
    var ceremonyInFlight = false
        private set

    /** True while the ceremony's parked message sits on Account. */
    var messagePending = false
        private set

    /**
     * True when the ceremony ended WITHOUT adopting a successor while the user
     * was on Account, so its outcome message is what the page shows — until the
     * user leaves. The refusal the ceremony causes has no fixed order against
     * the result (tui measured one landing ~30 ms after an undecidable result),
     * and escalating then would replace the message the user needs with an
     * import screen for a key they were never shown.
     */
    var outcomeOnScreen = false
        private set

    /**
     * How many Recovery kit sections are composed. A count, not a flag: a
     * re-entry can compose the new section BEFORE the old one leaves, and the
     * old one's leave edge is then not the user leaving Account (apple measured
     * exactly this on iOS, 2026-09-26).
     */
    private var accountMounts = 0

    /** True while the Account page (the Recovery kit section) is on screen. */
    val onAccount: Boolean get() = accountMounts > 0

    /** The escalation held back, if one is owed. */
    private var owed: (() -> Unit)? = null

    /** Whether an escalation is owed — for tests and the diagnostic log. */
    val isOwed: Boolean get() = owed != null

    /** Whether another Account-page writer may put [text] on screen: a clear
     *  always, an error only while no parked message is pending. */
    fun admits(text: String?): Boolean = text == null || !messagePending

    /** Route a supersession escalation: performed now, unless the ceremony owns
     *  the Account page, in which case it is recorded (latest wins — every
     *  arrival is the same teardown). */
    fun escalate(perform: () -> Unit) {
        if (ceremonyInFlight || outcomeOnScreen || messagePending) {
            ShellLog.i(TAG, "[supersession] held back: this device's own ceremony owns Account")
            owed = perform
            return
        }
        perform()
    }

    /** `identity-stolen-button` dispatched the ceremony. */
    fun ceremonyStarted() {
        ceremonyInFlight = true
    }

    /**
     * The ceremony's result was handled.
     *
     * - [adopted]: the device switched to the successor — the switch is itself
     *   the full relaunch, so an owed escalation is spent, not performed.
     * - [messageParked]: the parked message now sits on Account and holds the
     *   escalation until the user leaves it, wherever they are.
     *
     * Otherwise an owed escalation runs now if the user is off Account; on
     * Account the result stays on screen ([outcomeOnScreen]) and the leave edge
     * performs the escalation, after the user has read it.
     */
    fun ceremonyEnded(adopted: Boolean, messageParked: Boolean) {
        ceremonyInFlight = false
        messagePending = messageParked
        if (adopted) {
            owed = null
            return
        }
        outcomeOnScreen = onAccount
        if (!onAccount && !messagePending) performOwed()
    }

    /** A Recovery kit section entered composition. */
    fun accountAppeared() {
        accountMounts += 1
    }

    /**
     * A Recovery kit section left composition. When it was the last one, the
     * user left the Account page — the parked message's one acknowledgment
     * gesture, so it is discharged here and the held-back escalation, if any,
     * goes the ordinary way.
     */
    fun accountLeft() {
        accountMounts = maxOf(0, accountMounts - 1)
        if (accountMounts != 0) return
        messagePending = false
        outcomeOnScreen = false
        if (ceremonyInFlight) return
        performOwed()
    }

    private fun performOwed() {
        val perform = owed ?: return
        owed = null
        ShellLog.i(TAG, "[supersession] performing the held-back escalation")
        perform()
    }

    private companion object {
        const val TAG = "StolenCeremonyHold"
    }
}
