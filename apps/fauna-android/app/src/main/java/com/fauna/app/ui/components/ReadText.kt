package com.fauna.app.ui.components

import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/**
 * Read-only text can be selected and copied — the android leg of
 * `docs/goal/ui/README.md` § Text the person reads can be copied.
 *
 * The rule covers every piece of text the app *shows* rather than *acts on*:
 * messages, errors, status and outcome lines, explanatory copy, values and user
 * content. The app writes its texts at thousands of sites, so the rule is not a
 * per-site `SelectionContainer` (which the next new page would forget) but ONE
 * decision: [com.fauna.app.MainActivity] puts the whole app — every page,
 * onboarding and the locked surfaces included — inside this one
 * `SelectionContainer`, Compose's own selection (rule 1). The mirror of linux's
 * `read_text.rs`, which turns gtk's selection on for every label it maps.
 *
 * **What is a control, and so stays as it is (rule 3, selecting is not acting).**
 * A control's caption — a button's, a menu item's, a chip's, a tab's, a drawer
 * item's, a text field's label — is left out of the selection by the
 * same-named controls in [com.fauna.app.ui.controls], which wrap the caption in
 * `DisableSelection`; `ControlsSourceContractTest` holds every source file to
 * them. A clickable row or card whose text stays selectable keeps its click for
 * a press that makes no selection: Compose's selection consumes its own
 * long-press and drag, so the row's click never fires for a press that made a
 * selection, while an ordinary tap still acts on the row (`SelectableTextTest`).
 *
 * **A repaint keeps a selection (rule 2).** Compose keeps a selection across
 * recompositions that leave its text unchanged.
 */
@Composable
fun ReadTextRoot(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    SelectionContainer(modifier) { content() }
}
