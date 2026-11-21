package com.fauna.app.ui.screen.settings

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.SemanticsNodeInteraction
import androidx.compose.ui.test.assert
import com.fauna.app.R
import com.fauna.app.ui.util.AutomationAttributePack
import org.robolectric.RuntimeEnvironment
import uniffi.fauna_core.Possibility

/**
 * FFI-free stand-ins for the shared read answers (`Possibility::from_read` /
 * `for_write`, admin.md § Errors & edge cases, *Controls wait for their
 * read*): each carries the shared reason KEY, so a test asserts the painted
 * sentence; the rule itself is Rust-tested, these only pin the wiring. Plus
 * the one check every gated control owes the e2e drivers: the reason it
 * paints is published as its automation `reason` attribute.
 */
internal object ReadGateFakes {
    val stillLoading: Possibility = AdminDraftFakes.blocked("common.still_loading")
    val loadFailed: Possibility = AdminDraftFakes.blocked("common.load_failed")
    val inFlight: Possibility = AdminDraftFakes.blocked("common.in_flight")

    private fun str(id: Int) = RuntimeEnvironment.getApplication().getString(id)
    val stillLoadingText get() = str(R.string.common_still_loading)
    val loadFailedText get() = str(R.string.common_load_failed)
    val inFlightText get() = str(R.string.common_in_flight)

    /** The `stateDescription` [com.fauna.app.ui.util.paintedReason] publishes. */
    fun published(reason: String, state: String = ""): String =
        AutomationAttributePack.pack("reason" to reason, "state" to state)
}

/** Assert the node publishes [reason] as its automation `reason` attribute. */
internal fun SemanticsNodeInteraction.assertPaintedReason(reason: String, state: String = ""): SemanticsNodeInteraction =
    assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, ReadGateFakes.published(reason, state)))

/** Assert the node publishes no reason (a live control). */
internal fun SemanticsNodeInteraction.assertNoPaintedReason(): SemanticsNodeInteraction =
    assert(
        SemanticsMatcher("publishes no reason") { node ->
            val d = node.config.getOrElseNullable(SemanticsProperties.StateDescription) { null }
            d == null || !d.startsWith("reason=")
        },
    )
