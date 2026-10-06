package com.fauna.app.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.Card
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.ffi.FfiAccountEntry
import social.fauna.generated.Ids

/**
 * The multi-account switcher — the FIRST section on the Account settings page
 * (`long-term-store.md` § Multi-account evolution; ui.yaml `account-switcher-list`
 * and friends). Mirrors the linux (`apps/fauna-linux/src/settings/account.rs`) and
 * apple (`AccountSwitcherSection.swift`) references: rows come from the registry's
 * add order, the active row shows the indicator and is not tappable/removable, a
 * non-active row is tappable (switch) and carries a remove button.
 *
 * Stateless — no Hilt, no FFI native calls beyond the plain [FfiAccountEntry]
 * records the caller already read, so this composable is Robolectric-testable
 * with no host `.so` (mirrors [com.fauna.app.ui.screen.folders.FoldersContent]).
 *
 * Stage 2: each row carries the `account-require-confirm-toggle`
 * ([FfiAccountEntry.requireConfirmToActivate]) on EVERY row incl. the active one
 * (the natural target is the user's admin identity, often the active row).
 * Setting the flag never prompts; only *activating* a flagged account does — the
 * gate lives at the caller's switch handler (native BiometricPrompt), not here.
 */
@Composable
fun AccountSwitcherSection(
    accounts: List<FfiAccountEntry>,
    activeActorId: String?,
    label: (FfiAccountEntry) -> String,
    onSwitch: (actorId: String) -> Unit,
    onRemove: (actorId: String) -> Unit,
    onRequireConfirmToggle: (actorId: String, require: Boolean) -> Unit,
    onAddAccount: () -> Unit,
) {
    val requireConfirmLabel = stringResource(R.string.settings_account_page_require_confirm_toggle)
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp).testTag(Ids.ACCOUNT_SWITCHER_LIST),
        ) {
            Text(
                stringResource(R.string.settings_account_page_accounts),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                stringResource(R.string.settings_account_page_accounts_subtitle),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(8.dp))

            accounts.forEachIndexed { index, entry ->
                val isActive = entry.actorId == activeActorId
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag("account-switcher-item[$index]")
                        .then(
                            if (isActive) Modifier
                            else Modifier.clickable { onSwitch(entry.actorId) }
                        )
                        .padding(vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Text(
                        label(entry),
                        style = MaterialTheme.typography.bodyLarge,
                        modifier = Modifier
                            .weight(1f)
                            .testTag("account-item-handle[$index]"),
                    )
                    // Require-confirm-to-activate toggle — on EVERY row incl. the
                    // active one (long-term-store.md § Per-account re-auth). The
                    // Switch consumes its own tap, so flipping it does not trigger
                    // the row's switch-account click. Scoped within the row's
                    // account-switcher-item[$index] for the e2e's scoped query.
                    Switch(
                        checked = entry.requireConfirmToActivate,
                        onCheckedChange = { onRequireConfirmToggle(entry.actorId, it) },
                        modifier = Modifier
                            .testTag(Ids.ACCOUNT_REQUIRE_CONFIRM_TOGGLE)
                            .semantics { contentDescription = requireConfirmLabel },
                    )
                    Spacer(Modifier.width(8.dp))
                    if (isActive) {
                        Text(
                            stringResource(R.string.common_active),
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.testTag("account-item-active-indicator[$index]"),
                        )
                    } else {
                        IconButton(
                            onClick = { onRemove(entry.actorId) },
                            modifier = Modifier.testTag("account-remove-button[$index]"),
                        ) {
                            Icon(
                                Icons.Default.Delete,
                                contentDescription = stringResource(R.string.common_remove),
                            )
                        }
                    }
                }
                if (index < accounts.size - 1) HorizontalDivider()
            }

            HorizontalDivider()
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.ACCOUNT_ADD_BUTTON)
                    .clickable(onClick = onAddAccount)
                    .padding(vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(Icons.Default.Add, contentDescription = null)
                Spacer(Modifier.width(8.dp))
                Text(stringResource(R.string.settings_account_page_add_account))
            }
        }
    }
}
