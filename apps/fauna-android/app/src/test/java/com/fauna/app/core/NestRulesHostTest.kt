package com.fauna.app.core

import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.components.NestBlockWitness
import com.fauna.app.ui.components.NestPlaceholder
import com.fauna.app.ui.components.GuardianReason
import com.fauna.app.ui.components.RuleEditor
import com.fauna.app.ui.screen.settings.NestRulesCard
import com.fauna.ffi.FfiContentPolicy
import com.fauna.ffi.FfiNestPlaceholder
import com.fauna.ffi.FfiNestRulesBook
import com.fauna.ffi.FfiRegionSource
import com.fauna.ffi.FfiRuleEditor
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_client_policy.NestRuleLine
import uniffi.fauna_core.ContentLabelEntry
import uniffi.fauna_client_policy.NestRulesSummary
import uniffi.fauna_client_policy.RuleKind
import uniffi.fauna_core.LocalizedText

/**
 * The android leg of the nest rules (`nest-rules.md` § The nest source in the
 * engine, § The shared rule editor): the nest placeholder paints the
 * `nest-blocked-*` ids with the convention-17 witness sets in step, a session
 * nest routes the render through the plane's one composed call, the guardian
 * document written in the shared editor binds the ward's render, the rules
 * surface paints the shared summary, and the editor paint forwards to the
 * shared model. Verify, fold and the editor model are shared Rust, tested
 * there; this proves the android wiring.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class NestRulesHostTest {
    @get:Rule val compose = createComposeRule()
    @get:Rule val dir = TemporaryFolder()

    @After fun reset() {
        NestRulesHost.resetForTest()
        RegionPlaneHost.resetForTest()
    }

    private val subject = RegionSubject("ab".repeat(32), null, "a new dare tonight", emptyList(), false)

    @Test
    fun `a block placeholder paints the nest ids, offers no reveal and keeps the witness sets in step`() {
        val p = FfiNestPlaceholder(verb = "block", domain = "example.test", reason = "No gambling")
        compose.setContent {
            NestBlockWitness("feed:x", isBlocked = true)
            NestPlaceholder(p, witnessKey = "feed:x", onReveal = {})
        }
        compose.onNodeWithTag(Ids.NEST_BLOCKED_NOTICE)
            .assertTextEquals("Not shown on example.test — blocked under this nest’s rules")
        compose.onNodeWithTag(Ids.NEST_BLOCKED_DOMAIN).assertTextEquals("example.test")
        compose.onNodeWithTag(Ids.NEST_BLOCKED_REASON).assertTextEquals("No gambling")
        compose.onAllNodesWithTag(Ids.NEST_COLLAPSED_REVEAL_BUTTON).assertCountEquals(0)
        compose.waitForIdle()
        assertEquals(1 to 1, NestRulesHost.blockRenderState())
    }

    @Test
    fun `a collapse placeholder reveals and is not counted as a block`() {
        val p = FfiNestPlaceholder(verb = "collapse", domain = "example.test", reason = "Hidden")
        var revealed = false
        compose.setContent { NestPlaceholder(p, witnessKey = "feed:y", onReveal = { revealed = true }) }
        compose.onNodeWithTag(Ids.NEST_COLLAPSED_REVEAL_BUTTON).performClick()
        assertTrue(revealed)
        assertEquals(0 to 0, NestRulesHost.blockRenderState())
    }

    @Test
    fun `a session nest with no rules on the device renders as before`() {
        RegionPlaneHost.open(null, FfiRegionSource.SYSTEM_REGION, dir.root.absolutePath)
        assertNull(NestRulesHost.source())
        val before = RegionPlaneHost.render(emptyList(), ContentPolicyInputs(), subject)
        NestRulesHost.installForTest(FfiNestRulesBook(), "00")
        val after = RegionPlaneHost.render(emptyList(), ContentPolicyInputs(), subject)
        assertEquals(before, after)
        assertEquals("show", after.verdict)
        assertNull(after.nestPlaceholder)
        assertNull(NestRulesHost.summary())
    }

    /** A guardian document with one keywords rule over [term], written through
     *  the shared editor exactly as the guardian's mounting writes it. */
    private fun guardianPolicy(term: String): FfiContentPolicy {
        val editor = FfiRuleEditor.forGuardian(null)
        val row = editor.addRule(RuleKind.KEYWORDS)!!
        assertTrue(editor.setKeywordsInline(row, term))
        assertTrue(editor.setReason(row, editor.view().reasonDefaultKey, "Not for you"))
        assertTrue(editor.view().canSave)
        return FfiContentPolicy("inherit", "inherit", "inherit", "inherit", document = editor.documentBytes())
    }

    @Test
    fun `the guardian document written in the shared editor binds the wards render`() {
        RegionPlaneHost.open(null, FfiRegionSource.SYSTEM_REGION, dir.root.absolutePath)
        val inputs = ContentPolicyInputs(contentPolicy = guardianPolicy("dare"))
        assertEquals("block", RegionPlaneHost.render(emptyList(), inputs, subject).verdict)
        // The same through the nest-composed call, and an item the rule misses.
        NestRulesHost.installForTest(FfiNestRulesBook(), "00")
        assertEquals("block", RegionPlaneHost.render(emptyList(), inputs, subject).verdict)
        val other = subject.copy(text = "homework")
        assertEquals("show", RegionPlaneHost.render(emptyList(), inputs, other).verdict)
    }

    @Test
    fun `a guardian document rules reason rides the decision and paints beside the frame`() {
        RegionPlaneHost.open(null, FfiRegionSource.SYSTEM_REGION, dir.root.absolutePath)
        val inputs = ContentPolicyInputs(contentPolicy = guardianPolicy("dare"))
        val decision = RegionPlaneHost.render(emptyList(), inputs, subject)
        assertEquals("block", decision.guardianPlaceholder?.verb)
        assertEquals("Not for you", decision.guardianReason)
        compose.setContent { GuardianReason(decision.guardianReason) }
        compose.onNodeWithTag(Ids.CONTENT_POLICY_BLOCKED_REASON).assertTextEquals("Not for you")
        // The verdict a floor drove names no reason.
        val floor = ContentPolicyInputs(contentPolicy = FfiContentPolicy("block", "inherit", "inherit", "inherit"))
        val floored = RegionPlaneHost.render(
            listOf(ContentLabelEntry(category = "nsfw", confidencePerMille = 900.toUShort())), floor, subject.copy(text = "homework"),
        )
        assertEquals("block", floored.verdict)
        assertNull(floored.guardianReason)
    }

    @Test
    fun `no reason paints nothing`() {
        compose.setContent { GuardianReason(null) }
        compose.onAllNodesWithTag(Ids.CONTENT_POLICY_BLOCKED_REASON).assertCountEquals(0)
    }

    @Test
    fun `the rules surface paints the shared summary`() {
        val summary = NestRulesSummary(
            domain = "example.test",
            sequence = 2u,
            issuedAt = 1_700_000_000,
            rules = listOf(
                NestRuleLine(
                    verdict = LocalizedText("rule_editor.verdict.block", emptyMap()),
                    level = null,
                    reason = "No gambling",
                    inert = null,
                ),
                NestRuleLine(
                    verdict = LocalizedText("rule_editor.verdict.collapse", emptyMap()),
                    level = LocalizedText("rule_editor.level.standard", emptyMap()),
                    reason = "No spam",
                    inert = LocalizedText("rule_editor.prompt_not_applied", emptyMap()),
                ),
            ),
            notApplied = null,
        )
        compose.setContent { NestRulesCard(summary) }
        compose.onNodeWithTag(Ids.SETTINGS_NEST_RULES_SECTION).assertExists()
        compose.onAllNodesWithTag(Ids.SETTINGS_NEST_RULES_ITEM).assertCountEquals(2)
        compose.onAllNodesWithTag(Ids.SETTINGS_NEST_RULES_ITEM)[0].assertTextEquals("Block — No gambling")
        compose.onAllNodesWithTag(Ids.SETTINGS_NEST_RULES_ITEM)[1]
            .assertTextEquals("Hide behind a warning · Standard — No spam")
        compose.onAllNodesWithTag(Ids.SETTINGS_NEST_RULES_INERT_NOTICE).assertCountEquals(1)
        val version = compose.onNodeWithTag(Ids.SETTINGS_NEST_RULES_VERSION).fetchSemanticsNode()
            .config[androidx.compose.ui.semantics.SemanticsProperties.Text].joinToString("")
        assertTrue(version, version.startsWith("Version 2,"))
    }

    @Test
    fun `the editor paint adds a rule and shows the shared defect until it can save`() {
        val editor = FfiRuleEditor.forGuardian(null)
        compose.setContent {
            RuleEditor(
                editor = editor,
                writeKind = "fauna.family.policy.update",
                status = "",
                saving = false,
                onSave = {},
                onChange = {},
                loadLabelers = { emptyList() },
                loadModule = { null },
            )
        }
        compose.onAllNodesWithTag(Ids.RULE_EDITOR_ITEM).assertCountEquals(0)
        compose.onNodeWithTag(Ids.RULE_EDITOR_ADD_BUTTON).performClick()
        compose.onAllNodesWithTag(Ids.RULE_EDITOR_ITEM).assertCountEquals(1)
        assertEquals(1, editor.view().rows.size)
        // A new rule has no reason yet: the status paints the crate's own
        // defect sentences, and Save stays disabled.
        assertTrue(editor.view().defects.isNotEmpty())
        val status = compose.onNodeWithTag(Ids.RULE_EDITOR_STATUS).fetchSemanticsNode()
            .config[androidx.compose.ui.semantics.SemanticsProperties.Text].joinToString("")
        assertTrue(status, status.startsWith("Rule 1 "))
        compose.onNodeWithTag(Ids.RULE_EDITOR_SAVE_BUTTON)
            .assert(androidx.compose.ui.test.isNotEnabled())
    }
}
