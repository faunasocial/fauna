package com.fauna.app.ui.viewmodel

import android.content.Context
import com.fauna.app.ui.util.answerRefusal
import com.fauna.app.ui.util.inFlightPossibility
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import uniffi.fauna_core.Possibility

/**
 * One admin write that waits for its read (`docs/goal/behavior/admin.md`
 * § Errors & edge cases, *Controls wait for their read*), held by the page's
 * view-model: [answer] is what the control is painted from, and [begin] is the
 * press's half — it asks the SAME answer live and refuses with its reason,
 * so nothing reaches the nest while the read is pending, failed, or the
 * page's own change is still on its way.
 *
 * [live] is the shared answer (a machine's `toggle_possibility` /
 * `write_possibility` / `add_possibility`, or the page's own read through
 * `write_after_read_possibility`); this class adds only what the machine
 * cannot see from its side of the FFI: the view-model's own write between
 * the press and the re-read, painted with the shared in-flight answer
 * ([inFlightPossibility]). The twin of FaunaKit's `refusePress` + tui's
 * `admin::action_possibility`.
 */
class ReadGatedWrite(
    private val context: Context,
    private val live: () -> Possibility,
) {
    private val _answer = MutableStateFlow(live())
    val answer: StateFlow<Possibility> = _answer

    private var inFlight = false

    /** The answer asked NOW (what [begin] asks), for a page folding it into a
     *  wider answer of its own. */
    fun current(): Possibility = if (inFlight) inFlightPossibility() else live()

    /** Re-read the answer after the machine's snapshot moved. */
    fun refresh() {
        _answer.value = current()
    }

    /**
     * The press: `true` when the write may go (and it is now in flight until
     * [end]); `false` when refused, the reason handed to [onRefuse] (the
     * page's `error-message`) — an `Absent` answer refuses with no text.
     */
    fun begin(onRefuse: (String) -> Unit): Boolean {
        val refusal = answerRefusal(context, current())
        if (refusal != null) {
            refusal.reason?.let(onRefuse)
            return false
        }
        inFlight = true
        refresh()
        return true
    }

    /** The write came back (either way): the answer is the machine's again. */
    fun end() {
        inFlight = false
        refresh()
    }
}
