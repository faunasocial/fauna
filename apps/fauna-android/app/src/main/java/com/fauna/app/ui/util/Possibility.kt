package com.fauna.app.ui.util

import android.content.Context
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility

// `fauna_core::possibility::Possibility` on android — the one answer every
// gated affordance's shared-Rust predicate gives (`docs/goal/ui/README.md`
// § Offer only what is possible, rule 6). A screen never decides an action's
// enabled, disabled-with-reason or hidden state itself: it reads these three
// off the answer and hands them to `enabled =`, `DisabledControlReasonText`
// and its own `if`, the same three `fauna_core` methods tui and linux read.

/** `true` only for `Possible`: the value a composable's `enabled =` takes. */
val Possibility.isPossible: Boolean get() = this is Possibility.Possible

/** `false` only for `Absent`: whether the affordance is composed at all. */
val Possibility.isShown: Boolean get() = this !is Possibility.Absent

/** The reason to paint beside a disabled affordance; `null` when it is
 *  possible, and when it is absent (nothing is painted to explain). */
val Possibility.blockedReason: LocalizedText? get() = (this as? Possibility.Blocked)?.reason

/**
 * `fauna_core::possibility::Possibility::and` — the two answers an affordance
 * waits on, the page's first: `Absent` on either side is absent, else the
 * first `Blocked` wins, else [other]. A transcription of the shared combinator
 * (UniFFI carries the enum, not its methods), never a rule of its own.
 */
infix fun Possibility.and(other: Possibility): Possibility = when {
    this is Possibility.Absent || other is Possibility.Absent -> Possibility.Absent
    this is Possibility.Blocked -> this
    else -> other
}

/**
 * Rule 6's second half for an answer whose reason may carry a nested i18n key
 * (the features gate's `{window}`): the action asks the SAME answer its
 * control was painted from and refuses with this text on the page's
 * `error-message`. `null` = it may go; `Absent` refuses with no text (the
 * control was never offered). [formRefusal]'s nested twin.
 */
fun answerRefusal(context: Context, answer: Possibility): FormRefusal? = when (answer) {
    is Possibility.Possible -> null
    is Possibility.Blocked -> FormRefusal(resolveLocalizedNested(context, answer.reason))
    is Possibility.Absent -> FormRefusal(null)
}

/**
 * Rule 6 at the press, in one call: ask [answer] (the one the control was
 * painted from) again; possible → [go], blocked → its reason to [onRefuse]
 * (the page's `error-message`), absent → nothing.
 */
inline fun pressAsked(
    context: Context,
    answer: Possibility,
    onRefuse: (String) -> Unit,
    go: () -> Unit,
) {
    val refusal = answerRefusal(context, answer)
    if (refusal == null) go() else refusal.reason?.let(onRefuse)
}
