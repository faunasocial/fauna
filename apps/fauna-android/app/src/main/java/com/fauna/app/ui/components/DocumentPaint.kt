package com.fauna.app.ui.components

import android.view.View
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.text.TextLayoutResult
import java.util.concurrent.ConcurrentHashMap
import kotlin.math.roundToInt

/**
 * What each on-screen [DocumentBlocks] body actually painted, for the debug test agent's
 * `painted` read (`tests/e2e-unified/helpers/painted_lines.py`, the `document-painted-read`
 * surface): the one-line `get_text` drops a task item's box and a nested item's indent, so a
 * witness of render-model.md § D7 reads the body line by line off the laid-out text instead.
 *
 * Off unless [recording] — only the debug `TestAgent` turns it on — so a release body pays one
 * volatile read per layout pass and keeps nothing. Each body registers under its own key, with
 * its on-screen rect (pixels, the frame the automation bridge answers for its test tag) and its
 * last [TextLayoutResult]; [linesAt] finds the body under a frame's centre.
 */
object DocumentPaint {
    /** One leading space per this many dp of laid-out left offset —
     *  web's `WEB_INDENT_PX` in `painted_lines.py`, so the two read the same geometry. */
    const val INDENT_DP = 8f

    @Volatile var recording: Boolean = false

    private class Body {
        @Volatile var rect: IntArray? = null
        @Volatile var layout: TextLayoutResult? = null
        @Volatile var density: Float = 1f
    }

    private val bodies = ConcurrentHashMap<Any, Body>()

    /** The body's rect on screen, from its own coordinates and its window's screen offset. */
    fun place(key: Any, view: View, coords: LayoutCoordinates) {
        if (!recording || !coords.isAttached) return
        val bounds = coords.boundsInWindow()
        val origin = IntArray(2).also { view.getLocationOnScreen(it) }
        bodies.getOrPut(key) { Body() }.rect = intArrayOf(
            origin[0] + bounds.left.roundToInt(), origin[1] + bounds.top.roundToInt(),
            origin[0] + bounds.right.roundToInt(), origin[1] + bounds.bottom.roundToInt(),
        )
    }

    fun laidOut(key: Any, layout: TextLayoutResult, density: Float) {
        if (!recording) return
        val body = bodies.getOrPut(key) { Body() }
        body.layout = layout
        body.density = density
    }

    fun forget(key: Any) {
        bodies.remove(key)
    }

    /**
     * The painted lines of the body whose on-screen rect holds the centre of [frame]
     * (`x,y,w,h`, the bridge's `frame` read of the element), the smallest such rect when bodies
     * nest; `null` when no recorded body is there.
     */
    fun linesAt(frame: String): List<String>? {
        val parts = frame.split(',').mapNotNull { it.trim().toFloatOrNull() }
        if (parts.size != 4) return null
        val cx = parts[0] + parts[2] / 2
        val cy = parts[1] + parts[3] / 2
        val hit = bodies.values
            .mapNotNull { body -> body.rect?.let { r -> body to r } }
            .filter { (body, r) -> body.layout != null && cx >= r[0] && cx < r[2] && cy >= r[1] && cy < r[3] }
            .minByOrNull { (_, r) -> (r[2] - r[0]).toLong() * (r[3] - r[1]) }
            ?.first ?: return null
        val layout = hit.layout ?: return null
        val pxPerSpace = INDENT_DP * hit.density
        return paintedLines(layout.layoutInput.text.text, pxPerSpace) { offset ->
            layout.getHorizontalPosition(offset, usePrimaryDirection = true)
        }
    }

    /**
     * The painting rule, pure: one line per non-blank text line of [text], its own characters
     * (the `☑`/`☐`/bullet glyph included) with the lead whitespace replaced by one space per
     * [pxPerSpace] of the first visible character's laid-out left offset ([xOf], pixels against
     * the body's own left edge) — the geometry the user sees, so a body that painted its nesting
     * flat reads flat.
     */
    fun paintedLines(text: String, pxPerSpace: Float, xOf: (Int) -> Float): List<String> {
        val lines = mutableListOf<String>()
        var start = 0
        for (line in text.split('\n')) {
            val first = line.indexOfFirst { !it.isWhitespace() }
            if (first >= 0) {
                val x = xOf(start + first)
                val spaces = if (pxPerSpace > 0f) (x / pxPerSpace).roundToInt().coerceAtLeast(0) else 0
                lines += " ".repeat(spaces) + line.substring(first).trimEnd()
            }
            start += line.length + 1
        }
        return lines
    }
}
