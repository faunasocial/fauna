package com.fauna.app.ui.screen.profile

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import social.fauna.generated.Ids

/**
 * The Profile's **private section** (`profile.md` § The private section) — the
 * viewer's own nickname, notes and labels on the person being viewed. OTHER
 * only, below the relationship actions and above the tab strip.
 *
 * Stateless for the Robolectric harness (no VM, no FFI): the staged form and
 * every refusal come from shared Rust through
 * [com.fauna.app.ui.viewmodel.ProfilePrivateVM]; this owns the widgets only.
 * [onAddLabel] answers whether the label was staged — the add field empties
 * only then, so a refused label stays typed.
 */
@Composable
fun ProfilePrivateSection(
    nickname: String,
    notes: String,
    labels: List<String>,
    saving: Boolean,
    onNicknameChange: (String) -> Unit,
    onNotesChange: (String) -> Unit,
    onAddLabel: (String) -> Boolean,
    onRemoveLabel: (Int) -> Unit,
    onSave: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var labelInput by remember { mutableStateOf("") }
    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp)
            .testTag(Ids.PROFILE_PRIVATE_SECTION),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            stringResource(R.string.profile_private_title),
            style = MaterialTheme.typography.titleSmall,
        )
        OutlinedTextField(
            value = nickname,
            onValueChange = onNicknameChange,
            label = { Text(stringResource(R.string.profile_private_nickname)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().testTag(Ids.PROFILE_NICKNAME_FIELD),
        )
        OutlinedTextField(
            value = notes,
            onValueChange = onNotesChange,
            label = { Text(stringResource(R.string.profile_private_notes)) },
            minLines = 3,
            modifier = Modifier.fillMaxWidth().testTag(Ids.PROFILE_NOTES_FIELD),
        )
        Text(
            stringResource(R.string.profile_private_labels),
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Column(modifier = Modifier.fillMaxWidth().testTag(Ids.PROFILE_LABEL_LIST)) {
            labels.forEachIndexed { index, label ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        label,
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.weight(1f).testTag(Ids.PROFILE_LABEL_CHIP),
                    )
                    TextButton(
                        onClick = { onRemoveLabel(index) },
                        modifier = Modifier.testTag(Ids.PROFILE_LABEL_REMOVE_BUTTON),
                    ) { Text(stringResource(R.string.profile_private_label_remove)) }
                }
            }
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedTextField(
                value = labelInput,
                onValueChange = { labelInput = it },
                placeholder = { Text(stringResource(R.string.profile_private_label_add)) },
                singleLine = true,
                modifier = Modifier.weight(1f).testTag(Ids.PROFILE_LABEL_FIELD),
            )
            OutlinedButton(
                onClick = { if (onAddLabel(labelInput)) labelInput = "" },
                modifier = Modifier.testTag(Ids.PROFILE_LABEL_ADD_BUTTON),
            ) { Text(stringResource(R.string.profile_private_label_add)) }
        }
        Button(
            onClick = onSave,
            enabled = !saving,
            modifier = Modifier.testTag(Ids.PROFILE_PRIVATE_SAVE_BUTTON),
        ) { Text(stringResource(R.string.profile_private_save)) }
    }
}

/**
 * The Profile header's two names (`profile.md` § The private section → *The
 * header shows both names*): [primary] on `profile-handle`, and — only while a
 * nickname is the primary line — the public name it replaced on
 * `profile-public-name`. Stateless; both strings are the shared resolver's.
 */
@Composable
fun ProfileHeaderNames(primary: String, publicName: String?, modifier: Modifier = Modifier) {
    Column(modifier = modifier) {
        Text(
            primary,
            style = MaterialTheme.typography.titleMedium,
            maxLines = 1,
            overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            modifier = Modifier.testTag(Ids.PROFILE_HANDLE),
        )
        if (publicName != null) {
            Text(
                publicName,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                modifier = Modifier.testTag(Ids.PROFILE_PUBLIC_NAME),
            )
        }
    }
}
