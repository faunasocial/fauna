package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.core.NestRulesHost
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.ffi.FfiNestPlaceholder
import social.fauna.generated.Ids

/**
 * The nest placeholder painted **in place of** an item a rule of the nest it was
 * read through withholds (`nest-rules.md` § The nest source in the engine;
 * § Element IDs): the app's frame naming the nest (`nest-blocked-notice`), its
 * domain (`nest-blocked-domain`) and the admin's reason verbatim
 * (`nest-blocked-reason`, never an i18n string); a `collapse` adds the reveal
 * (`nest-collapsed-reveal-button`), which runs [onReveal] — the surface's own
 * family reveal. A `block` has no reveal: a member never loosens a nest rule.
 * [RegionPlaceholder]'s twin; tui's and web's nest branch.
 *
 * [witnessKey] names the surface + item for convention 17's painted side
 * ([NestRulesHost.witnessPainted]).
 */
@Composable
fun NestPlaceholder(
    placeholder: FfiNestPlaceholder,
    witnessKey: String,
    onReveal: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val isBlock = placeholder.verb == "block"
    DisposableEffect(witnessKey, isBlock) {
        NestRulesHost.witnessPainted(witnessKey, isBlock)
        onDispose { NestRulesHost.witnessPainted(witnessKey, false) }
    }
    Column(
        modifier = modifier.padding(vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(
            stringResourceFmt(
                if (isBlock) R.string.nest_rules_blocked_notice else R.string.nest_rules_collapsed_notice,
                placeholder.domain,
            ),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.NEST_BLOCKED_NOTICE),
        )
        Text(
            placeholder.domain,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.NEST_BLOCKED_DOMAIN),
        )
        Text(
            placeholder.reason,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.NEST_BLOCKED_REASON),
        )
        if (!isBlock) {
            TextButton(
                onClick = onReveal,
                modifier = Modifier.testTag(Ids.NEST_COLLAPSED_REVEAL_BUTTON),
            ) {
                Text(stringResource(R.string.nest_rules_reveal_button))
            }
        }
    }
}

/**
 * Convention 17's verdict side for one on-screen item and the nest source:
 * registers [key] while composed and a nest rule blocks it
 * ([NestRulesHost.witnessBlocked]) — [RegionBlockWitness]'s twin, called beside
 * it before any arm branches.
 */
@Composable
fun NestBlockWitness(key: String, isBlocked: Boolean) {
    DisposableEffect(key, isBlocked) {
        NestRulesHost.witnessBlocked(key, isBlocked)
        onDispose { NestRulesHost.witnessBlocked(key, false) }
    }
}
