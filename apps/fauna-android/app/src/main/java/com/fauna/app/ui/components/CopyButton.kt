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
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import android.content.Context
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.ui.util.getStringFmt
import com.fauna.app.ui.controls.OutlinedButton
import com.fauna.app.ui.controls.TextButton

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
 * iOS prior art these just copy by default; sites that surface a banner pass an
 * [onCopied] callback, and a [CopyButton] with `confirm` confirms ("Copied!") or
 * reports the failure itself.
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
 *
 * With [confirm] the button reports the write's outcome itself, the
 * `account-actor-id-copy-btn` contract (`docs/goal/ui/README.md` § Offer only
 * what is possible, rule 7): a write that reached the clipboard relabels a
 * labelled button `common.copied` ("Copied!") and paints the copied text as the
 * node's `copied` attribute (its `stateDescription`, as [CopyableRow] does); a
 * write that threw hands `common.copy_failed` (its `{error}` filled) to
 * [onCopyFailed], for the page's error line. [mayCopy] is the press asking the
 * control's answer again (rule 6) — `false` writes nothing; the caller painted
 * the refusal.
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
    confirm: Boolean = false,
    onCopyFailed: (String) -> Unit = {},
    mayCopy: () -> Boolean = { true },
) {
    val context = LocalContext.current
    val copy = rememberCopyToClipboard()
    // The text the last confirmed write put on the clipboard; keyed on the
    // text, so a new payload starts unconfirmed.
    var copied by remember(text) { mutableStateOf("") }
    val onClick: () -> Unit = onClick@{
        if (!mayCopy()) return@onClick
        if (!confirm) {
            copy(text)
            onCopied()
            return@onClick
        }
        val failure = runCatching { copy(text) }.exceptionOrNull()
        if (failure == null) {
            copied = text
            onCopied()
        } else {
            copied = ""
            onCopyFailed(copyFailedText(context, failure))
        }
    }
    val shownLabel = if (confirm && label != null && copied.isNotEmpty()) {
        stringResource(R.string.common_copied)
    } else {
        label
    }
    val tagged = modifier.testTag(testTag).then(
        if (confirm) Modifier.semantics { stateDescription = copied } else Modifier
    )
    when {
        shownLabel == null -> IconButton(onClick = onClick, enabled = enabled, modifier = tagged) {
            Icon(Icons.Default.ContentCopy, contentDescription = stringResource(R.string.common_copy))
        }
        outlined -> OutlinedButton(onClick = onClick, enabled = enabled, modifier = tagged) {
            Text(shownLabel)
        }
        else -> TextButton(onClick = onClick, enabled = enabled, modifier = tagged) {
            Text(shownLabel)
        }
    }
}

/** `common.copy_failed` with its `{error}` filled from [failure] — tui's `copy_failed_text`. */
fun copyFailedText(context: Context, failure: Throwable): String =
    context.getStringFmt(R.string.common_copy_failed, failure.message ?: failure.toString())

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
    // The row's `copied` attribute (its `stateDescription`) is the exact string
    // handed to the clipboard, never re-derived — the driver-facing witness of a
    // copy, since no driver reads the OS clipboard (same shape as the Profile
    // `profile-actor-id-copy-btn`). Keyed on the value, so it starts empty.
    var copied by remember(fullValue) { mutableStateOf("") }
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clickable {
                copy(fullValue)
                copied = fullValue
                onCopied()
            }
            .padding(vertical = 4.dp)
            .then(
                if (testTag != null) Modifier.testTag(testTag).semantics { stateDescription = copied }
                else Modifier
            ),
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
