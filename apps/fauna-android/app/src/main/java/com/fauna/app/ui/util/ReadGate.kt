package com.fauna.app.ui.util

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import com.fauna.ffi.readPossibility
import com.fauna.ffi.writeAfterReadPossibility
import uniffi.fauna_core.Possibility
import uniffi.fauna_core.ReadState

// `docs/goal/behavior/admin.md` § Errors & edge cases, *Controls wait for their
// read*, on android: a control that writes what a snapshot read decides is
// painted from the shared answer for that read (a machine's
// `toggle_possibility` / `write_possibility` / `add_possibility` /
// `confirm_possibility`, or the page's own read through `read_possibility` /
// `write_after_read_possibility`) — *still loading* before the read, *failed to
// load* after a failed one, *still saving* while its own change is on its way —
// and its press asks the same answer ([pressAsked]). Kotlin never spells any of
// the three reasons: every one is a shared `Possibility`.

/**
 * Paint a control from the read answer [read] (the page's first answer, so a
 * page still loading says so before any draft complaint), folded with the
 * control's own [draft] answer and the offline [gate]: the same three halves
 * [faunaFormGate] hands a form submit. Call `faunaGate(kind, enabled = …)` at
 * the call site so the kind stays a literal the offline-gate checker reads.
 */
@Composable
fun faunaReadGate(
    read: Possibility,
    gate: FaunaGateVerdict? = null,
    draft: Possibility = Possibility.Possible,
): FaunaFormVerdict = faunaFormGate(read and draft, gate)

/**
 * The answer a control driven by one shared machine gives: the machine's own
 * once it exists; a machine that could not be built means the page will never
 * read, so *failed to load* (`read_possibility(Failed)`), never a guess. The
 * twin of FaunaKit's `machinePossibility` and tui's `machine` helper.
 */
inline fun <M : Any> machinePossibility(machine: M?, ask: (M) -> Possibility): Possibility =
    if (machine != null) ask(machine) else readPossibility(ReadState.FAILED)

/**
 * A write this page sent and has not heard back from: the shared
 * `write_after_read_possibility` over a resolved read with the page busy —
 * *still saving*. Only ever asked after the press found the answer possible,
 * which already required the read to have resolved.
 */
fun inFlightPossibility(): Possibility = writeAfterReadPossibility(ReadState.RESOLVED, false)

/**
 * Publish the reason a control paints while it is blocked as its automation
 * `reason` attribute — what the e2e drivers' `painted_reason` reads (`reason`
 * on every app; tui's possibility seam publishes the same). `null` (a live
 * control) publishes nothing. A node that also paints a `state` of its own
 * (the mail-enable toggle's `on`/`off`/`unavailable`) hands it as [state]:
 * published bare while there is no reason, packed beside the reason
 * ([AutomationAttributePack]) while there is one. With no state of its own the
 * reason is packed beside an EMPTY `state` (an empty value reads as not set),
 * so a toggle's `state`/`checked` read still falls through to its live checked
 * flag and a node never publishes a bare reason another attribute read would
 * echo.
 */
fun Modifier.paintedReason(reason: String?, state: String? = null): Modifier = when {
    !reason.isNullOrEmpty() ->
        this.semantics { stateDescription = AutomationAttributePack.pack("reason" to reason, "state" to state.orEmpty()) }
    state != null -> this.semantics { stateDescription = state }
    else -> this
}
