package com.fauna.app.core.atproto

import uniffi.fauna_atproto_settings_machine.AtprotoSettingsMachine
import uniffi.fauna_atproto_settings_machine.depthLevelOptions
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility

/**
 * What the AT Protocol page may offer now (`docs/goal/ui/README.md` § Offer
 * only what is possible; `docs/goal/ui/atproto.md` § Errors & edge cases).
 *
 * Every answer here is the shared machine's own (`rung_possibility`,
 * `authorize_possibility` over UniFFI) — the page paints from them and never
 * re-derives a gate from the snapshot's deprecated `hostedAllowed` /
 * `hostedGateReason` fields. The two constants below exist only for the
 * moments NO machine can answer, and are the same two answers tui paints:
 * before the first read ([PENDING]) and when the machine failed to build
 * ([UNAVAILABLE]).
 *
 * @property unavailable the machine could not be built: the page paints one
 *   reason line and the way back instead of every control (rule 7).
 */
data class AtprotoPossibilities(
    val unavailable: Boolean,
    private val rungs: Map<String, Possibility>,
    val authorize: Possibility,
) {
    /** The answer for the rung issuing [level]. A level with no answer is
     *  blocked "still loading", never possible (rule 5: unknown is not
     *  possible). */
    fun rung(level: String): Possibility = rungs[level] ?: STILL_LOADING

    companion object {
        private val STILL_LOADING: Possibility =
            Possibility.Blocked(LocalizedText("common.still_loading", emptyMap()))

        /** Before the first machine answer: nothing is offered on a guess. */
        val PENDING = AtprotoPossibilities(false, emptyMap(), STILL_LOADING)

        /** The machine failed to build: the page offers nothing (rule 7). */
        val UNAVAILABLE = AtprotoPossibilities(true, emptyMap(), STILL_LOADING)

        /** The machine's answers, one per catalog rung. */
        fun of(machine: AtprotoSettingsMachine) = AtprotoPossibilities(
            unavailable = false,
            rungs = depthLevelOptions().associate { it.level to machine.rungPossibility(it.level) },
            authorize = machine.authorizePossibility(),
        )

        /** Test seam: a page whose every answer is given, never derived. */
        fun forTest(
            rungs: Map<String, Possibility> = emptyMap(),
            authorize: Possibility = Possibility.Possible,
            unavailable: Boolean = false,
        ) = AtprotoPossibilities(unavailable, rungs, authorize)
    }
}
