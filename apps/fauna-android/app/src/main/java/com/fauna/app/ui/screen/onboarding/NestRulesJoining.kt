package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.ui.screen.settings.ruleLine
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.stringResourceFmt
import social.fauna.generated.Ids
import uniffi.fauna_client_policy.NestRulesSummary

/**
 * `onboarding-nest-rules` — the joining surface (`nest-rules.md` § Acceptance at
 * joining): joining is acceptance, so `invite_request`, and `handle_entry` while
 * its Continue signs in, show the nest's rules directly above the terms notice.
 * A paint of the shared machine's `joiningNestRules` (which already decided
 * whether this page shows them): the heading naming the nest, the not-applied
 * sentence after it when none of the document applies here, then one
 * `onboarding-nest-rules-item` per rule, each line built as the settings surface
 * builds it. tui `nest_rules::joining_elements`, web `NestRulesJoining.svelte`.
 */
@Composable
fun NestRulesJoining(summary: NestRulesSummary) {
    val heading = stringResourceFmt(R.string.nest_rules_joining_heading, summary.domain)
    val text = summary.notAppliedSentence?.let { "$heading ${localized(it).orEmpty()}" } ?: heading
    Column(
        modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(
            text,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.testTag(Ids.ONBOARDING_NEST_RULES),
        )
        for (rule in summary.rules) {
            // A rule this device cannot apply carries its not-applied note on a
            // second line of the same item (tui `joining_rule_line`).
            val inert = rule.inert?.let { localized(it).orEmpty() }
            Text(
                if (inert != null) "${ruleLine(rule)}\n$inert" else ruleLine(rule),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.ONBOARDING_NEST_RULES_ITEM),
            )
        }
    }
}
