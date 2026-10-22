package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.core.NestRulesHost
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.stringResourceFmt
import social.fauna.generated.Ids
import uniffi.fauna_client_policy.NestRuleLine
import uniffi.fauna_client_policy.NestRulesSummary

/**
 * `settings-nest-rules-section` — the rules surface (`nest-rules.md`
 * § Acceptance at joining, and a change of rules → *The rules surface*): the
 * rules in force for the nest this session reads through — per rule the
 * verdict, the level and the nest's own reason — its version and issue time, and
 * any rule not applied on this device. A paint of the shared book's `summary`
 * (every label already chosen in `fauna_client_policy`), never an app-side fold
 * — [RegionSettingsSection]'s twin, beside it on the Status sub-page; tui's
 * `nest_rules::settings_elements`, web's `NestRulesSettingsSection.svelte`.
 *
 * Opening this surface is reading the change: it clears the one-line notice.
 * Absent while the nest has published no rules.
 */
@Composable
fun NestRulesSettingsSection() {
    LaunchedEffect(Unit) { NestRulesHost.dismissNotice() }
    val revision by NestRulesHost.revision.collectAsState()
    val summary = remember(revision) { NestRulesHost.summary() }
    if (summary == null || (summary.rules.isEmpty() && summary.notApplied == null)) return
    NestRulesCard(summary)
}

/** The section's card for [summary]. */
@Composable
internal fun NestRulesCard(summary: NestRulesSummary) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SETTINGS_NEST_RULES_SECTION)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                stringResourceFmt(R.string.nest_rules_settings_title, summary.domain),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                stringResourceFmt(
                    R.string.nest_rules_version,
                    summary.sequence.toString(),
                    ValueFormat.absoluteLocal(summary.issuedAt),
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.SETTINGS_NEST_RULES_VERSION),
            )
            for (rule in summary.rules) {
                Column(modifier = Modifier.padding(vertical = 4.dp)) {
                    Text(ruleLine(rule), modifier = Modifier.testTag(Ids.SETTINGS_NEST_RULES_ITEM))
                    rule.inert?.let { inert ->
                        Text(
                            localized(inert).orEmpty(),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.testTag(Ids.SETTINGS_NEST_RULES_INERT_NOTICE),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun ruleLine(rule: NestRuleLine): String {
    val verdict = localized(rule.verdict).orEmpty()
    val level = rule.level
    return if (level != null) {
        stringResourceFmt(R.string.nest_rules_rule_line, verdict, localized(level).orEmpty(), rule.reason)
    } else {
        stringResourceFmt(R.string.nest_rules_rule_line_no_level, verdict, rule.reason)
    }
}
