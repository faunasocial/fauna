package com.fauna.app.testing

import com.fauna.ffi.FfiContentPolicy
import com.fauna.ffi.FfiRuleEditor
import uniffi.fauna_client_policy.RuleKind
import uniffi.fauna_client_policy.RuleVerdict

/**
 * A guardian content policy for host tests, written through the shared rule
 * editor exactly as the guardian's mounting (`family-policy-rules-section`)
 * writes it: one category rule per entry of [rules] (category → verdict) at the
 * editor's default level (500 per-mille, the standard trigger), each with
 * [reason] as its required default-language reason. The guardian's rules are
 * a rule document (`family-safety.md` § Content policy); there are no
 * per-category floors to set.
 */
object GuardianRulesFixture {
    fun policy(vararg rules: Pair<String, RuleVerdict>, reason: String = "Not for you"): FfiContentPolicy {
        val editor = FfiRuleEditor.forGuardian(null)
        for ((category, verdict) in rules) {
            val row = checkNotNull(editor.addRule(RuleKind.CATEGORY)) { "the guardian editor refused a rule" }
            check(editor.setCategory(row, category)) { "category $category refused" }
            check(editor.setVerdict(row, verdict)) { "verdict $verdict refused" }
            check(editor.setReason(row, editor.view().reasonDefaultKey, reason)) { "reason refused" }
        }
        check(editor.view().canSave) { "the guardian document does not validate" }
        return FfiContentPolicy(document = editor.documentBytes())
    }

    /** A guardian policy that blocks [category]. */
    fun blocking(category: String) = policy(category to RuleVerdict.BLOCK)

    /** A guardian policy that collapses [category]. */
    fun collapsing(category: String) = policy(category to RuleVerdict.COLLAPSE)
}
