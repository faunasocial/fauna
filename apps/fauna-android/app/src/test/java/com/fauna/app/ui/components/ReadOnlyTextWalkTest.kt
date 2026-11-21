package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.platform.ClipboardManager
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalTextToolbar
import androidx.compose.ui.platform.TextToolbar
import androidx.compose.ui.platform.TextToolbarStatus
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.text.AnnotatedString
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.testing.InertMagnifierShadow
import com.fauna.app.ui.controls.AlertDialog
import com.fauna.app.ui.controls.Button
import com.fauna.app.ui.controls.OutlinedTextField
import com.fauna.app.ui.controls.TextButton
import com.fauna.app.ui.screen.KidsIneligibleScreen
import com.fauna.app.ui.screen.LaunchAccountLockedScreen
import com.fauna.app.ui.screen.LaunchRetryScreen
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * The walk that holds `docs/goal/ui/README.md` § Text the person reads can be
 * copied on android — the shape of linux's walk rule I10.
 *
 * Each case composes a real page inside [ReadTextRoot] (the one wrap
 * `MainActivity` puts every page in) and walks its semantics tree:
 *
 * - every text that sits outside any clickable node is READ text, and a
 *   long-press on it must select it — the observable is Compose's own copy
 *   action, which puts a piece of exactly that text on the clipboard (rule 1);
 * - every text that is a button's caption (its nearest clickable ancestor has
 *   `Role.Button`) is a control, and the same long-press must select nothing
 *   (rule 3, through [com.fauna.app.ui.controls]).
 *
 * The non-vacuity twin composes the same page WITHOUT the wrap and requires the
 * walk to flag its read texts, so a walk that cannot see a selection fails
 * rather than passes. The android and android-kids flavours share these pages
 * (`src/main`), so one walk holds both.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], shadows = [InertMagnifierShadow::class])
class ReadOnlyTextWalkTest {

    @get:Rule
    val rule = createComposeRule()

    private var copy: (() -> Unit)? = null

    private val clipboard = object : ClipboardManager {
        var held: AnnotatedString? = null
        override fun setText(annotatedString: AnnotatedString) {
            held = annotatedString
        }
        override fun getText(): AnnotatedString? = held
    }

    private val toolbar = object : TextToolbar {
        override var status: TextToolbarStatus = TextToolbarStatus.Hidden
            private set

        override fun showMenu(
            rect: Rect,
            onCopyRequested: (() -> Unit)?,
            onPasteRequested: (() -> Unit)?,
            onCutRequested: (() -> Unit)?,
            onSelectAllRequested: (() -> Unit)?,
        ) {
            copy = onCopyRequested
            status = TextToolbarStatus.Shown
        }

        override fun hide() {
            status = TextToolbarStatus.Hidden
        }
    }

    private fun render(wrapped: Boolean, page: @Composable () -> Unit) {
        rule.setContent {
            CompositionLocalProvider(
                LocalTextToolbar provides toolbar,
                LocalClipboardManager provides clipboard,
            ) {
                if (wrapped) ReadTextRoot { page() } else page()
            }
        }
        rule.waitForIdle()
    }

    private fun SemanticsNode.text(): String =
        config.getOrNull(SemanticsProperties.Text)?.joinToString("") { it.text }.orEmpty()

    /** The nearest node at or above this one that a click acts on, if any. */
    private fun SemanticsNode.clickableAncestor(): SemanticsNode? {
        var n: SemanticsNode? = this
        while (n != null) {
            if (n.config.contains(SemanticsActions.OnClick)) return n
            n = n.parent
        }
        return null
    }

    /** What one long-press on [node] put on the clipboard through Compose's copy. */
    private fun selectWithALongPress(node: SemanticsNode): String {
        clipboard.held = null
        copy = null
        rule.onNode(SemanticsMatcher("node ${node.id}") { it.id == node.id }, useUnmergedTree = true)
            .performTouchInput { longClick() }
        rule.waitForIdle()
        copy?.invoke()
        rule.waitForIdle()
        return clipboard.held?.text.orEmpty()
    }

    private data class Walk(val readTexts: List<String>, val captions: List<String>, val failures: List<String>)

    /** True when a text field (its label, placeholder or line) holds this text: the field's own caption. */
    private fun SemanticsNode.insideATextField(): Boolean {
        var n: SemanticsNode? = parent
        while (n != null) {
            if (n.config.contains(SemanticsProperties.EditableText)) return true
            n = n.parent
        }
        return false
    }

    private fun walk(): Walk {
        val nodes = rule.onAllNodes(
            SemanticsMatcher("shows text") {
                it.text().isNotBlank() && !it.config.contains(SemanticsProperties.EditableText)
            },
            useUnmergedTree = true,
        ).fetchSemanticsNodes()
        val read = mutableListOf<String>()
        val captions = mutableListOf<String>()
        val failures = mutableListOf<String>()
        for (node in nodes) {
            val shown = node.text()
            if (node.insideATextField()) continue
            val clickable = node.clickableAncestor()
            when {
                clickable == null -> {
                    read += shown
                    val copied = selectWithALongPress(node)
                    if (copied.isEmpty() || !shown.contains(copied.trim())) {
                        failures += "read text not selectable: \"$shown\" (copied \"$copied\")"
                    }
                }
                clickable.config.getOrNull(SemanticsProperties.Role) == Role.Button -> {
                    captions += shown
                    val copied = selectWithALongPress(node)
                    if (copied.isNotEmpty()) {
                        failures += "control caption selectable: \"$shown\" (copied \"$copied\")"
                    }
                }
            }
        }
        return Walk(read, captions, failures)
    }

    private fun assertConforms(walk: Walk, minRead: Int, minCaptions: Int = 0) {
        assertTrue("the walk reached read text: ${walk.readTexts}", walk.readTexts.size >= minRead)
        assertTrue("the walk reached control captions: ${walk.captions}", walk.captions.size >= minCaptions)
        assertEquals(emptyList<String>(), walk.failures)
    }

    // ── Real pages.

    @Test
    fun kidsIneligiblePage() {
        render(wrapped = true) { KidsIneligibleScreen(onSignOut = {}, error = "Sign-out failed: boom") }
        assertConforms(walk(), minRead = 2, minCaptions = 1)
    }

    @Test
    fun launchAccountLockedPage() {
        render(wrapped = true) { LaunchAccountLockedScreen(unlockTime = "2027-01-02 03:04", onStolen = {}) }
        assertConforms(walk(), minRead = 1, minCaptions = 1)
    }

    @Test
    fun launchRetryPage() {
        render(wrapped = true) {
            LaunchRetryScreen(onRetry = {}, onUseDifferentNest = {}, recoverableBoxes = emptyList(), onRetire = {})
        }
        assertConforms(walk(), minRead = 1, minCaptions = 2)
    }

    /** A status line is read text; the shadowed button's caption is a control. */
    @Test
    fun aShadowedButtonBesideAStatusLine() {
        render(wrapped = true) {
            Column {
                Text("A status line the person may paste")
                Button(onClick = {}) { Text("Save the thing") }
                OutlinedTextField(value = "", onValueChange = {}, label = { Text("Field label") })
            }
        }
        assertConforms(walk(), minRead = 1, minCaptions = 1)
    }

    /**
     * A dialog is another window. The page's selection reaching into it throws
     * on the first long-press ("layouts are not part of the same hierarchy"),
     * which is why [com.fauna.app.ui.controls.AlertDialog] and
     * [com.fauna.app.ui.controls.Dialog] give the dialog a selection of its own:
     * the same long-press on the shadowed dialog's text goes through. (Whether
     * it then selects is not observable here: the dialog's window brings its
     * own text toolbar, which the test cannot stand in for.)
     */
    @Test
    fun aDialogsTextTakesALongPressInsideThePageSelection() {
        render(wrapped = true) {
            AlertDialog(
                onDismissRequest = {},
                confirmButton = { TextButton(onClick = {}) { Text("Understood") } },
                title = { Text("Dialog heading words") },
                text = { Text("The nest refused the request: quota exceeded") },
            )
        }
        rule.onNodeWithText("The nest refused the request: quota exceeded").performTouchInput { longClick() }
        rule.waitForIdle()
        rule.onNodeWithText("Dialog heading words").performTouchInput { longClick() }
        rule.waitForIdle()
    }

    /**
     * A menu is a popup window too: [com.fauna.app.ui.controls.DropdownMenu]
     * keeps its content (a heading line among the items) out of the page's
     * selection, so a long-press on it goes through.
     */
    @Test
    fun aMenusTextTakesALongPressInsideThePageSelection() {
        render(wrapped = true) {
            androidx.compose.foundation.layout.Box {
                com.fauna.app.ui.controls.DropdownMenu(expanded = true, onDismissRequest = {}) {
                    Text("React with")
                    com.fauna.app.ui.controls.DropdownMenuItem(text = { Text("Heart") }, onClick = {})
                }
            }
        }
        rule.onNodeWithText("React with").performTouchInput { longClick() }
        rule.waitForIdle()
        rule.onNodeWithText("Heart").performTouchInput { longClick() }
        rule.waitForIdle()
    }

    /** The twin of the dialog case: material3's own dialog inside the page's selection throws. */
    @Test
    fun theUnshadowedDialogThrowsInsideThePageSelection() {
        render(wrapped = true) {
            androidx.compose.material3.AlertDialog(
                onDismissRequest = {},
                confirmButton = {},
                text = { Text("The nest refused the request: quota exceeded") },
            )
        }
        val thrown = runCatching {
            rule.onNodeWithText("The nest refused the request: quota exceeded").performTouchInput { longClick() }
            rule.waitForIdle()
        }.exceptionOrNull()
        assertTrue("the unshadowed dialog threw: $thrown", thrown is IllegalArgumentException)
    }

    // ── The non-vacuity twin: the same page without the wrap is flagged.

    @Test
    fun withoutTheWrapTheWalkFlagsTheReadText() {
        render(wrapped = false) { KidsIneligibleScreen(onSignOut = {}, error = "Sign-out failed: boom") }
        val w = walk()
        assertTrue("the walk reached read text: ${w.readTexts}", w.readTexts.size >= 2)
        assertTrue(
            "every read text is flagged without the wrap: ${w.failures}",
            w.failures.count { it.startsWith("read text not selectable") } == w.readTexts.size,
        )
    }
}
