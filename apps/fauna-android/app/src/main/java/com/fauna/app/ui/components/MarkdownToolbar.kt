package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconToggleButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import social.fauna.generated.Ids

/**
 * Compose-bar markdown toolbar (`docs/goal/ui/conversations.md` § Where logic lives). Each button
 * wraps the body field's *selection* in inline markers. The wrap **rule** — where the
 * `*`/`**`/`` ` ``/`[…](url)` markers go, keeping edge whitespace OUTSIDE them so a
 * word-selection's trailing space can't produce `*italic *` — lives once in shared Rust
 * (`fauna_core::markdown::wrap_selection`, reached via the `wrapMarkdownSelection` UniFFI face)
 * and is injected as [wrap]; this composable only does the per-widget *splice* ([applyWrap]).
 * Priority #1/#2/#4 (one rule across all seven apps). The default [naiveMarkdownWrap] is the
 * FFI-free fallback for previews/tests — production screens inject the shared rule.
 *
 * [markersShown]/[onToggleMarkers] drive the per-editor marker-visibility toggle
 * (`markdown-marker-toggle-button`, docs/goal/ui/conversations.md § Compose-field inline markdown styling):
 * default hidden (caret-edge reveal via `compose_decoration_plan`); pressed reveals every marker
 * dimmed (the existing all-dimmed live preview). Client-local, no persistence — mirrors web's
 * `MarkdownToolbar.svelte`. Omit [onToggleMarkers] to hide the button (matches web's optional-prop
 * behavior for surfaces with no marker-visibility concept, e.g. a plain feed/article textarea).
 */
@Composable
fun MarkdownToolbar(
    value: TextFieldValue,
    onValueChange: (TextFieldValue) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    wrap: MarkdownWrap = naiveMarkdownWrap,
    markersShown: Boolean = false,
    onToggleMarkers: (() -> Unit)? = null,
) {
    fun wrapSelection(prefix: String, suffix: String, placeholder: String) {
        val sel = value.text.substring(value.selection.min, value.selection.max)
        onValueChange(applyWrap(value, wrap(sel, prefix, suffix, placeholder)))
    }
    Row(modifier = modifier.testTag(Ids.MARKDOWN_TOOLBAR), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        IconButton(onClick = { wrapSelection("**", "**", "") }, enabled = enabled, modifier = Modifier.testTag(Ids.MARKDOWN_BOLD_BUTTON)) {
            Icon(Icons.Default.FormatBold, "Bold", modifier = Modifier.size(20.dp))
        }
        IconButton(onClick = { wrapSelection("*", "*", "") }, enabled = enabled, modifier = Modifier.testTag(Ids.MARKDOWN_ITALIC_BUTTON)) {
            Icon(Icons.Default.FormatItalic, "Italic", modifier = Modifier.size(20.dp))
        }
        IconButton(onClick = { wrapSelection("`", "`", "") }, enabled = enabled, modifier = Modifier.testTag(Ids.MARKDOWN_CODE_BUTTON)) {
            Icon(Icons.Default.Code, "Code", modifier = Modifier.size(20.dp))
        }
        // Link: same shared wrap rule with the `[`…`](url)` markers (mirrors web's
        // `insert('[', '](url)')`); an empty selection becomes `[text](url)` with "text" selected.
        IconButton(onClick = { wrapSelection("[", "](url)", "text") }, enabled = enabled, modifier = Modifier.testTag(Ids.MARKDOWN_LINK_BUTTON)) {
            Icon(Icons.Default.Link, "Link", modifier = Modifier.size(20.dp))
        }
        if (onToggleMarkers != null) {
            IconToggleButton(
                checked = markersShown,
                onCheckedChange = { onToggleMarkers() },
                enabled = enabled,
                modifier = Modifier.testTag(Ids.MARKDOWN_MARKER_TOGGLE_BUTTON),
            ) {
                Icon(
                    if (markersShown) Icons.Default.Visibility else Icons.Default.VisibilityOff,
                    "Toggle marker visibility",
                    modifier = Modifier.size(20.dp),
                )
            }
        }
    }
}
