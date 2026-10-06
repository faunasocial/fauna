package com.fauna.app.ui.components

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/**
 * Un-id'd hint text naming why an adjacent disabled control is dead
 * (`ui/README.md` § Copy comprehensibility rule 5: "every disabled control the
 * user can see has an on-screen reason within eyeshot"). Chrome, not an
 * addressable element — no `.testTag`, mirroring the existing un-id'd hint
 * `Text` at `MailAliasesScreen.kt`'s `mail_aliases_no_default_domain` /
 * `MailListsScreen.kt`'s `mail_lists_no_domain`.
 *
 * Renders nothing when [reason] is null or empty, so a shared-machine getter
 * resolved through [com.fauna.app.ui.util.localized] (which returns null when
 * there's nothing to show) can be passed straight through — the verdict
 * (enabled/disabled) and its explanation come from the same paired shared-Rust
 * getters (e.g. `dns_provider_eligible` / `dns_provider_ineligible_reason`),
 * so they can't disagree.
 */
@Composable
fun DisabledControlReasonText(reason: String?, modifier: Modifier = Modifier) {
    if (!reason.isNullOrEmpty()) {
        Text(
            text = reason,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            modifier = modifier,
        )
    }
}
