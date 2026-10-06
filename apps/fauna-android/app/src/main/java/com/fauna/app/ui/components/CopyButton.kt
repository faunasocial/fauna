package com.fauna.app.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.dp
import com.fauna.app.R

/**
 * Shared copy-to-clipboard primitives — the Android twin of iOS
 * `FaunaKit.CopyButton` (priority #2: shared code in the platform family's own
 * layer; #3: same concept everywhere). Every screen that needs a `*-copy-btn`
 * affordance uses [CopyButton] (or [CopyableRow] for a tappable label/value row)
 * instead of hand-rolling the clipboard idiom — which had drifted into two
 * divergent flavors (`ClipboardManager.setPrimaryClip` vs.
 * `LocalClipboardManager.setText`) across ≥10 screens before this consolidation.
 *
 * The single clipboard write lives in [rememberCopyToClipboard]; the visible
 * affordances delegate to it, so the bug-prone part exists exactly once. Like the
 * iOS prior art these just copy — no ephemeral "Copied!" feedback; sites that
 * surface a banner pass an [onCopied] callback.
 */

/**
 * The one canonical clipboard write. Returns a `(String) -> Unit` that copies its
 * argument as plain text. Use directly for the rare non-button affordance (e.g. a
 * tappable value `Text`); prefer [CopyButton] / [CopyableRow] otherwise.
 */
@Composable
fun rememberCopyToClipboard(): (String) -> Unit {
    val clipboard = LocalClipboardManager.current
    return remember(clipboard) { { text -> clipboard.setText(AnnotatedString(text)) } }
}

/**
 * A copy-to-clipboard button carrying [testTag] (the canonical ui.yaml `*-copy-btn`
 * id). With no [label] it renders as an icon button (the `ContentCopy` glyph,
 * mirroring iOS's `doc.on.doc`); with a [label] it renders a text button —
 * [outlined] picks `OutlinedButton` (default) vs `TextButton`. [onCopied] fires
 * after the write for sites that surface a "copied" banner.
 */
@Composable
fun CopyButton(
    testTag: String,
    text: String,
    modifier: Modifier = Modifier,
    label: String? = null,
    outlined: Boolean = true,
    enabled: Boolean = true,
    onCopied: () -> Unit = {},
) {
    val copy = rememberCopyToClipboard()
    val onClick = {
        copy(text)
        onCopied()
    }
    when {
        label == null -> IconButton(onClick = onClick, enabled = enabled, modifier = modifier.testTag(testTag)) {
            Icon(Icons.Default.ContentCopy, contentDescription = stringResource(R.string.common_copy))
        }
        outlined -> OutlinedButton(onClick = onClick, enabled = enabled, modifier = modifier.testTag(testTag)) {
            Text(label)
        }
        else -> TextButton(onClick = onClick, enabled = enabled, modifier = modifier.testTag(testTag)) {
            Text(label)
        }
    }
}

/**
 * A full-width, tappable label/value row that copies [fullValue] (showing the
 * possibly-truncated [displayValue]). The row itself is the copy affordance —
 * [testTag] (the `*-copy-btn` id) goes on the row, [valueTestTag] on the value.
 * Folds together the formerly-duplicated `CopyRow` (Status) / `CopyableRow`
 * (Account settings) private composables. [onCopied] fires after the write.
 */
@Composable
fun CopyableRow(
    label: String,
    displayValue: String,
    fullValue: String,
    modifier: Modifier = Modifier,
    testTag: String? = null,
    valueTestTag: String? = null,
    onCopied: () -> Unit = {},
) {
    val copy = rememberCopyToClipboard()
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clickable {
                copy(fullValue)
                onCopied()
            }
            .padding(vertical = 4.dp)
            .then(if (testTag != null) Modifier.testTag(testTag) else Modifier),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(label, style = MaterialTheme.typography.bodyMedium)
        Text(
            displayValue,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = if (valueTestTag != null) Modifier.testTag(valueTestTag) else Modifier,
        )
    }
}
