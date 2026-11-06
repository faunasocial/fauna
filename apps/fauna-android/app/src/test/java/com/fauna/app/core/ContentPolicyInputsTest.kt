package com.fauna.app.core

import com.fauna.app.testing.GuardianRulesFixture
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_policy.RuleVerdict
import uniffi.fauna_core.ContentLabelEntry

/**
 * The android content-policy render engine ([ContentPolicyInputs.verdictFor],
 * family-safety.md § Content policy) — the twin of linux `content_policy::tests`
 * / the wasm `contentRenderVerdict` arms. Confirms the strictest-wins compose the
 * feed + conversation surfaces both call: the guardian's rules and the viewer's
 * own spam/phishing thresholds, resolved entirely in shared Rust.
 *
 * [ContentPolicyInputs.verdictFor] calls the real `contentRenderVerdict` over
 * UniFFI whenever a policy or threshold is present, so this runs via
 * `just android-host-test` (host JNA at `libfauna_ffi.so`), like
 * [com.fauna.app.ui.screen.settings.FamilyContentTest]. A guardian rule the
 * shared editor writes triggers at its default level, 500 per-mille
 * (`fauna_core::obligation::GUARDIAN_FLOOR_TRIGGER_PERMILLE`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ContentPolicyInputsTest {

    private fun label(category: String, permille: Int) =
        ContentLabelEntry(category = category, confidencePerMille = permille.toUShort())

    @Test
    fun `no policy and no thresholds short-circuits to show without the shared call`() {
        // The FFI-free path (the VM-free conversation content harness depends on
        // it): with no guardian policy and no own threshold nothing can hide, so
        // even a labeled item resolves "show" (badge/show are identical to the gate).
        val inputs = ContentPolicyInputs()
        assertEquals("show", inputs.verdictFor(emptyList()))
        assertEquals("show", inputs.verdictFor(listOf(label("nsfw", 900))))
    }

    @Test
    fun `a guardian block rule hides a flagged item`() {
        val inputs = ContentPolicyInputs(contentPolicy = GuardianRulesFixture.blocking("nsfw"))
        assertEquals("block", inputs.verdictFor(listOf(label("nsfw", 700))))
        // A clean item under a block rule still shows.
        assertEquals("show", inputs.verdictFor(emptyList()))
    }

    @Test
    fun `a guardian collapse rule collapses a flagged item`() {
        val inputs = ContentPolicyInputs(contentPolicy = GuardianRulesFixture.collapsing("spam"))
        assertEquals("collapse", inputs.verdictFor(listOf(label("spam", 700))))
    }

    @Test
    fun `the viewer's own spam threshold collapses for every viewer`() {
        // Own threshold at 500 per-mille, no guardian — the every-user un-darking
        // of moderation.md item 1. Both thresholds must be present (they always
        // are: one FfiSpamPreferences carries both).
        val inputs = ContentPolicyInputs(ownSpamPermille = 500u, ownPhishingPermille = 500u)
        assertEquals("collapse", inputs.verdictFor(listOf(label("spam", 700))))
    }

    @Test
    fun `guardian block wins over an own collapse threshold, strictest-wins`() {
        val inputs = ContentPolicyInputs(
            contentPolicy = GuardianRulesFixture.policy("spam" to RuleVerdict.BLOCK),
            ownSpamPermille = 500u,
            ownPhishingPermille = 500u,
        )
        // The own threshold would collapse spam; the guardian rule blocks it —
        // block is strictest.
        assertEquals("block", inputs.verdictFor(listOf(label("spam", 700))))
    }
}
