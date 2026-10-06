package com.fauna.app.core

import com.fauna.ffi.FfiContentPolicy
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_core.ContentLabelEntry

/**
 * The android content-policy render engine ([ContentPolicyInputs.verdictFor],
 * family-safety.md § Content policy) — the twin of linux `content_policy::tests`
 * / the wasm `contentRenderVerdict` arms. Confirms the strictest-wins compose the
 * feed + conversation surfaces both call: the guardian floor and the viewer's own
 * spam/phishing thresholds, resolved entirely in shared Rust.
 *
 * [ContentPolicyInputs.verdictFor] calls the real `contentRenderVerdict` over
 * UniFFI whenever a policy or threshold is present, so this runs via
 * `just android-host-test` (host JNA at `libfauna_ffi.so`), like
 * [com.fauna.app.ui.screen.settings.FamilyContentTest]. The GUARDIAN_FLOOR
 * trigger is 500 per-mille (`fauna_core::obligation::GUARDIAN_FLOOR_TRIGGER_PERMILLE`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ContentPolicyInputsTest {

    private fun label(category: String, permille: Int) =
        ContentLabelEntry(category = category, confidencePerMille = permille.toUShort())

    private fun floors(nsfw: String, spam: String, phishing: String, commercial: String) =
        FfiContentPolicy(nsfw = nsfw, spam = spam, phishing = phishing, commercial = commercial)

    @Test
    fun `no policy and no thresholds short-circuits to show without the shared call`() {
        // The FFI-free path (the VM-free conversation content harness depends on
        // it): with no floor and no own threshold nothing can hide, so even a
        // labeled item resolves "show" (badge/show are identical to the gate).
        val inputs = ContentPolicyInputs()
        assertEquals("show", inputs.verdictFor(emptyList()))
        assertEquals("show", inputs.verdictFor(listOf(label("nsfw", 900))))
    }

    @Test
    fun `a guardian block floor hides a flagged item`() {
        val inputs = ContentPolicyInputs(contentPolicy = floors("block", "inherit", "inherit", "inherit"))
        assertEquals("block", inputs.verdictFor(listOf(label("nsfw", 700))))
        // A clean item under a block floor still shows.
        assertEquals("show", inputs.verdictFor(emptyList()))
    }

    @Test
    fun `a guardian collapse floor collapses a flagged item`() {
        val inputs = ContentPolicyInputs(contentPolicy = floors("inherit", "collapse", "inherit", "inherit"))
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
            contentPolicy = floors("inherit", "block", "inherit", "inherit"),
            ownSpamPermille = 500u,
            ownPhishingPermille = 500u,
        )
        // The own threshold would collapse spam; the guardian floor blocks it —
        // block is strictest.
        assertEquals("block", inputs.verdictFor(listOf(label("spam", 700))))
    }

    @Test
    fun `an unparseable floor value renders fail-closed to block`() {
        // A floor value this build cannot name (a newer nest) folds to block.
        val inputs = ContentPolicyInputs(contentPolicy = floors("weirdnewvalue", "inherit", "inherit", "inherit"))
        assertEquals("block", inputs.verdictFor(listOf(label("nsfw", 600))))
    }
}
