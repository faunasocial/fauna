package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Box
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription

/**
 * A token-round-tripping select — every ui.yaml `select` whose contract is a
 * RAW wire value (`event-detail-reminder-select`'s contract, `ui/conversations.md`
 * § Element IDs): the driver picks and reads back the TOKEN, while the human
 * only ever sees the localized label. Compose has no native `<select>`, so
 * this is the expanded-menu shape every such picker on this app shares —
 * the folders member-access select, the backup destination kind, the room
 * editor's two rules, and the publish sheet's kind.
 *
 * What makes the token readable by the androidTest bridge without ever
 * painting it (the user-ruled option, 2026-09-26: a bridge extension, never
 * a raw string shown as the label):
 *
 * - the anchor carries `Role.DropdownList`, so its accessibility class is
 *   `android.widget.Spinner` — the widget type the bridge's `get_text` keys
 *   on to answer the token instead of the visible label — and `stateDescription
 *   = selected`, the token itself (GTK's model/display split, an HTML
 *   `<option value>`, a WinUI `ComboBoxItem.Tag`: the same value/label
 *   separation the other six apps' drivers read);
 * - every menu item carries `stateDescription = token`, which is how the
 *   bridge's `select(id, token)` finds an option by its VALUE, falling back to
 *   the visible text only for label-based pickers.
 *
 * Read side of that contract: `androidTest/.../bridge/ElementOps.kt`; the
 * pure rules it applies: `debug/.../testing/AutomationSemantics.kt`.
 *
 * [options] is `(token, label)` in paint order; [selected] is a token, and a
 * token no option carries (a value a newer app wrote) shows as itself rather
 * than as blank — the same rule the shared kind catalogs use.
 */
@Composable
fun TokenSelect(
    testTagValue: String,
    selected: String,
    options: List<Pair<String, String>>,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    var expanded by remember { mutableStateOf(false) }
    Box(modifier = modifier) {
        OutlinedButton(
            onClick = { expanded = true },
            enabled = enabled,
            // The token is what a driver reads back, never the label.
            modifier = Modifier
                .testTag(testTagValue)
                .semantics {
                    role = Role.DropdownList
                    stateDescription = selected
                },
        ) {
            Text(options.firstOrNull { it.first == selected }?.second ?: selected)
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { (token, label) ->
                DropdownMenuItem(
                    text = { Text(label) },
                    // The option's VALUE, for a driver selecting by token.
                    modifier = Modifier.semantics { stateDescription = token },
                    onClick = {
                        expanded = false
                        onSelect(token)
                    },
                )
            }
        }
    }
}
