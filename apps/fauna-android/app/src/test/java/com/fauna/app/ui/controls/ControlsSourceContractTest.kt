package com.fauna.app.ui.controls

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Every app source file calls the text-captioned material controls through
 * [com.fauna.app.ui.controls] — the half of `docs/goal/ui/README.md` § Text the
 * person reads can be copied that keeps a control's caption a control (rule 3)
 * while the whole app sits in one selection
 * ([com.fauna.app.ui.components.ReadTextRoot]).
 *
 * A file that calls one of [SHADOWED] must import it from this package by name
 * (that explicit import outranks `androidx.compose.material3.*`), and no file
 * may import material3's own or call it fully qualified. The controls with no
 * shadow — `SegmentedButton` (a row-scope extension), `NavigationDrawerItem`
 * (material3 keeps its default shape internal) and `ModalBottomSheet`'s content —
 * are wrapped at their sites, and [HAND_WRAPPED] holds each such
 * file to carrying the wrap.
 */
class ControlsSourceContractTest {

    private val sources: List<File> =
        File("src").listFiles()!!
            .filter { it.isDirectory && !it.name.startsWith("test") && !it.name.startsWith("androidTest") }
            .flatMap { dir -> dir.walkTopDown().filter { it.isFile && it.extension == "kt" }.toList() }
            .filterNot { it.invariantSeparatorsPath.contains("/ui/controls/") }

    private fun String.withoutImports() = lines().filterNot { it.startsWith("import ") }.joinToString("\n")

    @Test
    fun theWalkSeesTheApp() {
        assertTrue("found ${sources.size} source files", sources.size > 200)
    }

    @Test
    fun everyTextCaptionedControlComesFromTheControlsPackage() {
        val violations = mutableListOf<String>()
        for (file in sources) {
            val text = file.readText()
            val body = text.withoutImports()
            for ((name, origin) in SHADOWED) {
                if (Regex("^import ${Regex.escape(origin)}\\.$name$", RegexOption.MULTILINE).containsMatchIn(text)) {
                    violations += "${file.path}: imports $origin.$name"
                }
                if (body.contains("$origin.$name(")) {
                    violations += "${file.path}: calls $origin.$name directly"
                }
                val calls = Regex("(?<![\\w.])$name\\s*\\(").containsMatchIn(body)
                if (calls && !text.contains("import com.fauna.app.ui.controls.$name\n")) {
                    violations += "${file.path}: calls $name without `import com.fauna.app.ui.controls.$name`"
                }
            }
        }
        assertEquals(emptyList<String>(), violations)
    }

    @Test
    fun theUnshadowedControlsAreWrappedAtTheirSites() {
        val violations = mutableListOf<String>()
        for (file in sources) {
            val body = file.readText().withoutImports()
            for ((name, wrap) in HAND_WRAPPED) {
                val calls = Regex("(?<![\\w.])$name\\s*\\(").findAll(body).count()
                if (calls > 0 && Regex("\\b$wrap\\b").findAll(body).count() < calls) {
                    violations += "${file.path}: $calls $name call(s) but fewer `$wrap` wraps"
                }
            }
        }
        assertEquals(emptyList<String>(), violations)
    }

    private companion object {
        const val M3 = "androidx.compose.material3"

        val SHADOWED = listOf(
            "Button", "TextButton", "OutlinedButton", "FilledTonalButton", "DropdownMenuItem",
            "FilterChip", "AssistChip", "InputChip", "SuggestionChip", "Tab",
            "OutlinedTextField", "TextField", "AlertDialog", "DropdownMenu", "ExposedDropdownMenuBox",
        ).map { it to M3 } + ("Dialog" to "androidx.compose.ui.window")

        val HAND_WRAPPED = mapOf(
            "SegmentedButton" to "DisableSelection",
            "NavigationDrawerItem" to "DisableSelection",
            "ModalBottomSheet" to "SelectionContainer",
        )
    }
}
