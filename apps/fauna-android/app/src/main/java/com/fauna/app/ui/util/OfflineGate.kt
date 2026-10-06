package com.fauna.app.ui.util

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.staticCompositionLocalOf
import com.fauna.ffi.FfiConnectionState
import com.fauna.ffi.connectionStateWord
import com.fauna.ffi.offlineAffordance

// W4 (account-data-plane.md § Workstreams) phase 4's UI desensitizing for android — the android leg of the
// offline-mutation contract (`docs/goal/architecture/account-data-plane.md`
// § The offline-mutation contract → *How a surface asks*).
//
// A control that issues a wire kind declares it with `faunaGate` and gets both
// halves from ONE verdict: the `enabled` it hands its own composable, and the
// localized reason to render beside itself with `DisabledControlReasonText`.
//
// ## Compose is web's seam shape, reached from the other side
//
// The four legs before this one each took the shape their toolkit forced.
// **tui** rebuilds its element list every frame, so it gates inside the one
// function that returns that list (`App::page_elements`) and a page author
// writes no gate code at all. **linux** registers per widget and re-decides on
// a `notify::sensitive`, because a GTK widget outlives the state that gated it.
// **apple** applies one modifier, because SwiftUI's `.disabled` is a
// propagating, non-revocable environment that performs the composition for it.
// **web** hands the gate the call site's own predicate, because a Svelte
// `disabled=` is an effect that would otherwise rewrite the property underneath
// any registry.
//
// Compose lands on web's shape from the opposite direction. Like SwiftUI it
// re-evaluates from state, so linux's stale-widget problem cannot arise and
// nothing needs re-visiting. But unlike SwiftUI it has **no propagating
// disable**: `enabled` is an ordinary parameter of `Button`/`TextField`/…, not
// an environment a modifier can intercept, and `Modifier.semantics { disabled() }`
// marks a node for accessibility without greying it or blocking its click. So
// there is nothing for a modifier to wrap and no element list to gate — the
// verdict has to reach the `enabled =` argument itself, which is why this is a
// `@Composable` returning a value rather than a `Modifier`. web got here
// because a registry would be fought by the framework; android because there is
// nothing to register against.
//
// What the shape does NOT do is let a call site re-derive the decision: the
// boolean returned is already composed with the page's own intent, so nobody
// writes `if (!available)` and nobody can invert a ruling by accident.
//
// ## The rule is not reimplemented here
//
// The verdict is `offlineAffordance`, the UniFFI face of
// `fauna_protocol::offline_class::affordance`. Its three rulings — only class 3
// desensitizes, an unregistered kind stays available, only *known* offline
// words count as offline — are deliberately not restated in Kotlin: that
// per-app copy is what priority #2 forbids, and ruling 3 in particular fails
// SILENTLY IN THE DANGEROUS DIRECTION when a copy drifts. The connection word
// likewise comes from `connectionStateWord` rather than a Kotlin `when`, for
// the same reason.
//
// A kind is a plain string on this side of the boundary, so a typo reads as
// *available* (ruling 2) and quietly ungates the very control the declaration
// was written to gate. A dedicated dev-fleet checker (`offline-gate-check`)
// catches that — the string-literal apps' twin of tui's walk invariant I7.

/** The word handed to the shared rule when this app does not know its transport
 *  state — see [LocalConnectionState] for why that is not `"connecting"`. */
private const val UNKNOWN_STATE = "unknown"

/**
 * The live nest connection state for the whole shell, provided once by
 * `FaunaNavHost` off the same [com.fauna.app.ui.viewmodel.ConnectionStatusVM]
 * the `connection-status` indicator reads — so the indicator and the gate
 * cannot disagree about what "connected" means.
 *
 * ⚠ **`null` is UNKNOWN, never "connecting".** `connecting` is a *known*
 * offline word, so defaulting to it would grey every gated control wherever
 * this local does not reach — a `@Preview`, a composable rendered before the
 * shell provides it, a unit test that seeds no state — and grey it while the
 * nest is perfectly reachable. Handing the shared rule a word it does not know
 * is exactly what ruling 3 exists for: an unrecognised state leaves the control
 * live, because for a *gate* the honest answer to "we do not know" is **do not
 * block the user** (at worst the control shows the error it would have shown
 * anyway). Deliberately not defaulted to `CONNECTED` either: that would assert
 * a fact we do not have. This mirrors apple's missing-`FaunaClient` reasoning
 * (`FaunaKit/Core/OfflineGate.swift`) exactly.
 *
 * `staticCompositionLocalOf` because the transport state changes rarely and
 * every gated control must see the change — the same choice
 * `LocalSnackbarHostState`/`LocalAppMessages` already make in this app.
 */
val LocalConnectionState: ProvidableCompositionLocal<FfiConnectionState?> =
    staticCompositionLocalOf { null }

/**
 * One control's verdict: the `enabled` to hand the composable, and the reason
 * to render beside it when this gate is what disabled it.
 *
 * Both halves come from a single [faunaGate] call precisely so a caller cannot
 * ask twice and get two answers that disagree.
 */
@Immutable
data class FaunaGateVerdict(
    /**
     * The page's own intent **AND** the shared verdict. Pass it straight to the
     * composable's `enabled =`; never re-test the affordance beside it.
     *
     * The gate never *enables* what the page disabled — the page's own reason
     * ("saving…", "the form is incomplete") is stronger and more specific than
     * "no nest", the same rule tui's early return, linux's registry and web's
     * action all state. A reconnect therefore restores exactly the page's
     * intent, never more.
     */
    val enabled: Boolean,
    /**
     * The localized reason, non-null **only** when this gate is what disabled
     * the control — per affordance, never a global "you are offline" banner
     * (§ R11), which is why one mechanism also covers a nest-*less* account.
     *
     * `null` when the control is live, and `null` when the *page* had already
     * disabled it: the page renders its own, more specific reason there, and
     * two reasons stacked under one dead control is worse than either alone.
     */
    val reason: String?,
)

/**
 * Ask whether this control may be offered right now.
 *
 * ```kotlin
 * val gate = faunaGate("fauna.backup.destination.remove")
 * Button(
 *     onClick = onConfirm,
 *     enabled = gate.enabled,
 *     modifier = Modifier.testTag("backup-destination-remove-confirm-button"),
 * ) { Text(…) }
 * DisabledControlReasonText(gate.reason)
 * ```
 *
 * @param kind the wire kind this control issues. Deliberately **not** nullable:
 *   a control that issues no kind — a cancel, a reveal, a pure-local toggle —
 *   does not call this function at all, the same shape apple's modifier has.
 *   A `faunaGate(null)` would also read to the offline-gate-kinds checker as a
 *   declaration site with no kind literal, which is the one thing it cannot
 *   distinguish from a computed kind it must refuse. Where the kind genuinely
 *   turns on a discriminant, pass the same expression the action takes
 *   (`faunaGate(if (managed) "fauna.tls.publish_cert" else "fauna.account.state.put")`)
 *   and let the shared table decide which path stays live — never a Kotlin
 *   class test.
 * @param enabled the predicate the call site would otherwise have written into
 *   `enabled =`, handed over so this function is that argument's only author.
 *
 * Fails **open** on any throw from the shared face — the native library absent
 * under a `@Preview` or a host JVM without it. Same polarity as ruling 3, for
 * the same reason: a gate that cannot reach its verdict must not block the
 * user. (This cannot green a red test: every assertion that a control *is*
 * gated needs the call to have succeeded.)
 */
@Composable
fun faunaGate(kind: String, enabled: Boolean = true): FaunaGateVerdict {
    val state = LocalConnectionState.current
    val verdict = try {
        val word = if (state == null) UNKNOWN_STATE else connectionStateWord(state)
        offlineAffordance(kind, word)
    } catch (_: Throwable) {
        null
    }

    // `null` is the fail-open path, so it must read as NOT gated — spelled
    // `== false` rather than `!verdict.available` so an unreachable rule can
    // never be mistaken for a refusal.
    val gated = verdict?.available == false
    // The reason is resolved only when this gate is the one that closed the
    // control: `enabled == false` from the page means the page owns the
    // explanation.
    val reason = if (gated && enabled) localized(verdict?.reason) else null
    return FaunaGateVerdict(enabled = enabled && !gated, reason = reason)
}
