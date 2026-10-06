package com.fauna.app.ui.components

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.ParagraphStyle
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.OffsetMapping
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.input.TransformedText
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextIndent
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp

/**
 * Inline markdown styling + shared toolbar-wrap glue for the conversations compose field
 * (`docs/goal/ui/conversations.md` § Compose-field inline markdown styling + § Where logic lives).
 *
 * The buffer literally holds **markdown source** — `hello *world*` is what is stored, drafted,
 * and sent — and a decoration layer styles its byte ranges (Obsidian / Typora "live preview",
 * **not** WYSIWYG). The two shared-Rust definitions both live behind the `fauna-ffi` surface:
 *  - reading: `decoration_map(src) -> [FfiMdDecoration]` (the same inline scanner that feeds
 *    `parse_markdown`, so editor preview and the rendered message can never disagree);
 *  - writing: `wrap_markdown_selection(...)` (the toolbar wrap *rule*, keeping edge whitespace
 *    OUTSIDE the markers — see [MarkdownWrapResult]).
 *
 * Everything in this file is **FFI-free and pure** so the stateless [ComposeBar] /
 * [MarkdownToolbar] (and their Robolectric tests, which can't load the native `.so`) never
 * touch the FFI: the stateful screen maps the FFI returns into these plain types and injects
 * them. Android twin of `apps/fauna-linux/src/views/conversations/compose_decoration.rs`
 * (decoration) + `compose_toolbar.rs::wrap_selection` (wrap).
 */

/**
 * FFI-free mirror of `com.fauna.ffi.FfiMdDecoration` — one inline-styling decoration over a
 * **UTF-8 byte** range of the raw compose source (`decoration_map`'s offset unit). [kind] is
 * the snake_case `MdDecorationKind` token (`marker`, `bold`, `italic`, `bold_italic`, `code`,
 * `link`, `image`, `heading`, `blockquote`, `list_marker`); [level] is the heading level (1–4)
 * when `kind == "heading"`, else 0.
 */
data class MarkdownDecoration(
    val start: Int,
    val end: Int,
    val kind: String,
    val level: Int,
)

/**
 * FFI-free mirror of `com.fauna.ffi.FfiSelectionWrap` — the result of the shared
 * `wrap_selection` rule. The caller splices [replacement] over the selection, then re-selects
 * [core] by shifting the selection start past [beforeCore]'s length (see [applyWrap]).
 */
data class MarkdownWrapResult(
    val replacement: String,
    val beforeCore: String,
    val core: String,
)

/** The shared toolbar-wrap rule, injected so the stateless toolbar stays FFI-free. */
typealias MarkdownWrap = (selected: String, prefix: String, suffix: String, placeholder: String) -> MarkdownWrapResult

/**
 * FFI-free fallback wrap used by previews / Robolectric Content tests when the shared rule
 * isn't injected. Brackets the (placeholder-filled) selection with the markers; the
 * edge-whitespace-safe production rule lives in shared Rust (`wrap_selection`) and is injected
 * by the stateful screen, so this naive form never runs in production.
 */
val naiveMarkdownWrap: MarkdownWrap = { sel, prefix, suffix, placeholder ->
    val core = sel.ifBlank { placeholder }
    MarkdownWrapResult(replacement = prefix + core + suffix, beforeCore = prefix, core = core)
}

/**
 * Apply a [MarkdownWrapResult] to [value]: splice [MarkdownWrapResult.replacement] over the
 * current selection and re-select [MarkdownWrapResult.core]. Pure (offsets are Kotlin UTF-16
 * `String` indices, matching `TextFieldValue` selection units), so it's unit-testable without
 * the FFI.
 */
fun applyWrap(value: TextFieldValue, w: MarkdownWrapResult): TextFieldValue {
    val start = value.selection.min
    val end = value.selection.max
    val newText = value.text.substring(0, start) + w.replacement + value.text.substring(end)
    val coreStart = start + w.beforeCore.length
    return TextFieldValue(newText, TextRange(coreStart, coreStart + w.core.length))
}

/** UTF-8 byte length of a Unicode code point — the offset unit `decoration_map` returns. */
private fun utf8Len(codePoint: Int): Int = when {
    codePoint < 0x80 -> 1
    codePoint < 0x800 -> 2
    codePoint < 0x10000 -> 3
    else -> 4
}

/**
 * Convert a UTF-8 byte offset (decoration_map's unit, over the raw Rust `str`) to a UTF-16
 * `String` index (Compose's offset unit). Decoration offsets always fall on a char boundary,
 * so this lands exactly; clamps to the string length defensively.
 */
internal fun utf8ByteToUtf16Index(text: String, byteOffset: Int): Int {
    if (byteOffset <= 0) return 0
    var bytes = 0
    var i = 0
    while (i < text.length) {
        if (bytes >= byteOffset) break
        val cp = text.codePointAt(i)
        bytes += utf8Len(cp)
        i += Character.charCount(cp)
    }
    return i
}

/**
 * Inverse of [utf8ByteToUtf16Index] — convert a UTF-16 `String` index (Compose's caret/selection
 * unit) to a UTF-8 byte offset (the unit `compose_decoration_plan` / `compose_show_markers_dim_ranges`
 * take for `caret`). Mirrors windows' `Utf16IndexToUtf8Byte` (`ComposeMarkdownDecorator.cs`).
 */
internal fun utf16IndexToUtf8Byte(text: String, utf16Offset: Int): Int {
    val o = utf16Offset.coerceIn(0, text.length)
    var bytes = 0
    var i = 0
    while (i < o) {
        val cp = text.codePointAt(i)
        bytes += utf8Len(cp)
        i += Character.charCount(cp)
    }
    return bytes
}

/**
 * FFI-free mirror of `com.fauna.ffi.FfiRevealSpan` — a **UTF-8 byte** range into the raw compose
 * source (`compose_decoration_plan` / `compose_show_markers_dim_ranges`'s offset unit).
 */
data class RevealSpan(val start: Int, val end: Int)

/**
 * FFI-free mirror of `com.fauna.ffi.FfiComposeMarkerPlan` — the shared hide-by-default rule's
 * output. [hide] markers are concealed from the displayed text; [dim] markers (structural
 * prefixes off the caret's line, plus inline markers revealed by the caret-edge rule) render
 * dimmed. Both are **UTF-8 byte** ranges.
 */
data class ComposeMarkerPlan(val hide: List<RevealSpan>, val dim: List<RevealSpan>)

/**
 * The visual [SpanStyle] for a content decoration kind, or `null` for marker kinds (handled
 * separately so they can be revealed near the caret rather than always styled). Mirrors the
 * render path's styling in [DocumentBlocks] so the compose preview reads like the sent message.
 */
private fun contentStyle(
    kind: String,
    level: Int,
    linkColor: Color,
    codeBackground: Color,
): SpanStyle? = when (kind) {
    "bold" -> SpanStyle(fontWeight = FontWeight.Bold)
    "italic" -> SpanStyle(fontStyle = FontStyle.Italic)
    "bold_italic" -> SpanStyle(fontWeight = FontWeight.Bold, fontStyle = FontStyle.Italic)
    "code" -> SpanStyle(fontFamily = FontFamily.Monospace, background = codeBackground)
    "link", "image" -> SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline)
    "heading" -> {
        val size = when (level) {
            1 -> 1.5
            2 -> 1.3
            3 -> 1.15
            else -> 1.05
        }
        SpanStyle(fontWeight = FontWeight.Bold, fontSize = size.em)
    }
    "blockquote" -> SpanStyle(fontStyle = FontStyle.Italic)
    else -> null // marker / list_marker / unknown
}

/**
 * The quote indent (`conversations.md` § Compose-field inline markdown styling: "quote
 * indent"): a paragraph-level 12 sp inset over the whole quoted line — the 12 pt / 12 px
 * apple and windows give their compose quotes. Paragraph, not span: an indent is a property
 * of the line, and a paragraph style that ended mid-line would break the line in two.
 */
private val QuoteIndent = ParagraphStyle(textIndent = TextIndent(firstLine = 12.sp, restLine = 12.sp))

/**
 * The UTF-16 `[start, end)` range of every line of [text] holding a `blockquote` decoration
 * (which spans only the content after `> `), each line once, in order — the ranges
 * [QuoteIndent] covers. The end stops before the line's `\n`, so two quoted lines give two
 * paragraphs that touch but never overlap.
 */
internal fun quoteLineRanges(text: String, decorations: List<MarkdownDecoration>): List<Pair<Int, Int>> =
    decorations
        .filter { it.kind == "blockquote" }
        .map { d ->
            val s = utf8ByteToUtf16Index(text, d.start)
            val lineStart = if (s == 0) 0 else text.lastIndexOf('\n', s - 1) + 1
            val lineEnd = text.indexOf('\n', s).let { if (it < 0) text.length else it }
            lineStart to lineEnd
        }
        .filter { (s, e) -> s < e }
        .distinct()
        .sortedBy { it.first }

/**
 * One run of the styling the compose field APPLIED — its text and the looks it sets against
 * the field defaults, in linux's `get_attr(dm-text-field, "text-runs")` shape (one tag per
 * run): [weight] 700 for bold, [family] `monospace` for code, [scale] a heading's size factor,
 * [leftMargin] a quote's indent in sp; a default reads `null`. Concealed markers are spliced
 * out of the displayed text rather than tagged invisible, so no run carries them.
 */
data class AppliedRun(
    val text: String,
    val weight: Int?,
    val family: String?,
    val scale: Float?,
    val leftMargin: Float?,
)

/**
 * Split [applied] — the text the field displays, exactly as its [VisualTransformation]
 * handed it over — into [AppliedRun]s: a new run wherever a span or paragraph style starts
 * or ends and at every line break (a line break is no run). Pure, so it is unit-testable
 * without a field; the TestAgent's `compose_text_runs` serializes it over what
 * [ComposeFieldStyling] recorded.
 */
fun appliedRuns(applied: AnnotatedString): List<AppliedRun> {
    val text = applied.text
    val cuts = sortedSetOf(0, text.length)
    for (r in applied.spanStyles) { cuts += r.start; cuts += r.end }
    for (r in applied.paragraphStyles) { cuts += r.start; cuts += r.end }
    text.forEachIndexed { i, c -> if (c == '\n') { cuts += i; cuts += i + 1 } }
    return cuts.toList().zipWithNext().mapNotNull { (a, b) ->
        if (a >= b || b > text.length) return@mapNotNull null
        val piece = text.substring(a, b)
        if (piece == "\n") return@mapNotNull null
        val spans = applied.spanStyles.filter { it.start <= a && it.end >= b }.map { it.item }
        val paragraphs = applied.paragraphStyles.filter { it.start <= a && it.end >= b }.map { it.item }
        val weight = spans.mapNotNull { it.fontWeight?.weight }.maxOrNull()
            ?.takeIf { it >= FontWeight.Bold.weight }
        val family = if (spans.any { it.fontFamily == FontFamily.Monospace }) "monospace" else null
        val scale = spans.map { it.fontSize }.firstOrNull { it.isEm }?.value?.takeIf { it != 1f }
        val indent = paragraphs.mapNotNull { it.textIndent?.firstLine }
            .firstOrNull { it.isSp && it.value > 0f }?.value
        AppliedRun(piece, weight, family, scale, indent)
    }
}

/**
 * The one place the conversations compose field publishes what it applied: every filter of
 * its [VisualTransformation] records the displayed [AnnotatedString] here (see
 * [RecordingVisualTransformation]), and the field clears it when it leaves the composition.
 * Read by the debug TestAgent's `compose_text_runs` — android's twin of linux's
 * `gtk::TextBuffer` read, apple's `NSTextStorage` read and windows's RichEdit read — so the
 * e2e asserts what the field painted, never a recomputation of the shared decoration plan.
 * At most one compose field is on screen at a time; the last filter wins.
 */
object ComposeFieldStyling {
    @Volatile var applied: AnnotatedString? = null
}

/** Wraps the compose field's styling so each filter's output lands in [ComposeFieldStyling]. */
class RecordingVisualTransformation(private val inner: VisualTransformation) : VisualTransformation {
    override fun filter(text: AnnotatedString): TransformedText =
        inner.filter(text).also { ComposeFieldStyling.applied = it.text }
}

/**
 * Build the inline-styled [AnnotatedString] for the compose field's SHOW-MARKERS (all-visible,
 * dimmed live-preview) mode, from the raw markdown [text], the shared `decoration_map` ranges
 * ([decorations], **byte** offsets), and the shared `compose_show_markers_dim_ranges` rule's
 * output ([dimRanges], also **byte** offsets — matched by decoration **start** byte, mirroring
 * windows' `HashSet<ulong> dimStarts` idiom). Content ranges get a visual style; marker ranges
 * in [dimRanges] are dimmed ([markerColor]); every other marker (the caret's own line) shows
 * un-dimmed so the raw markers can be edited (Obsidian's "source on the active line"). Decoration
 * only adds styles — the text content/length is unchanged — so the caller's
 * [MarkdownVisualTransformation] uses an identity offset mapping.
 */
fun buildComposeDecoration(
    text: String,
    decorations: List<MarkdownDecoration>,
    dimRanges: List<RevealSpan>,
    linkColor: Color,
    markerColor: Color,
    codeBackground: Color,
): AnnotatedString = buildAnnotatedString {
    append(text)
    if (decorations.isEmpty()) return@buildAnnotatedString
    val dimStarts = dimRanges.map { it.start }.toHashSet()
    for (d in decorations) {
        val s = utf8ByteToUtf16Index(text, d.start)
        val e = utf8ByteToUtf16Index(text, d.end)
        if (s >= e || e > text.length) continue
        val style = contentStyle(d.kind, d.level, linkColor, codeBackground)
        if (style != null) {
            addStyle(style, s, e)
        } else if (d.start in dimStarts) {
            // Marker: dimmed off the caret's line; revealed (un-styled) on the active line.
            addStyle(SpanStyle(color = markerColor), s, e)
        }
    }
    for ((s, e) in quoteLineRanges(text, decorations)) addStyle(QuoteIndent, s, e)
}

/**
 * [VisualTransformation] that applies [buildComposeDecoration] over the field's text. Pure
 * styling (no insert/delete) → [OffsetMapping.Identity], so caret/selection math is unchanged.
 */
class MarkdownVisualTransformation(
    private val decorations: List<MarkdownDecoration>,
    private val dimRanges: List<RevealSpan>,
    private val linkColor: Color,
    private val markerColor: Color,
    private val codeBackground: Color,
) : VisualTransformation {
    override fun filter(text: AnnotatedString): TransformedText {
        val annotated = buildComposeDecoration(
            text.text, decorations, dimRanges, linkColor, markerColor, codeBackground,
        )
        return TransformedText(annotated, OffsetMapping.Identity)
    }
}

/**
 * Offset mapping for a [VisualTransformation] that removes ([hideRangesUtf16]) ranges from the
 * displayed text — the Compose analogue of web's atomic `Decoration.replace` ranges / linux's
 * GTK `invisible` TextTag (`docs/goal/ui/conversations.md` § Compose-field inline markdown
 * styling). [hideRangesUtf16] are UTF-16 `String` index ranges (`start` inclusive, `end`
 * exclusive), sorted, non-overlapping.
 *
 * `compose_decoration_plan`'s caret-edge-reveal rule guarantees a hidden range never contains
 * the caret (a run the caret sits inside is moved to `dim` instead), so a probed offset always
 * lands in a **kept** segment or exactly on a segment boundary — this never has to decide where
 * "inside a hidden run" maps to.
 */
internal class ConcealingOffsetMapping(
    hideRangesUtf16: List<IntRange>,
    originalLength: Int,
) : OffsetMapping {
    private data class Segment(val originalStart: Int, val transformedStart: Int, val length: Int)

    private val segments: List<Segment> = buildList {
        var cursor = 0
        var transformedCursor = 0
        for (r in hideRangesUtf16) {
            if (r.first > cursor) {
                val len = r.first - cursor
                add(Segment(cursor, transformedCursor, len))
                transformedCursor += len
            }
            cursor = maxOf(cursor, r.last + 1)
        }
        add(Segment(cursor, transformedCursor, (originalLength - cursor).coerceAtLeast(0)))
    }

    val transformedLength: Int = segments.sumOf { it.length }

    override fun originalToTransformed(offset: Int): Int {
        val seg = segments.lastOrNull { it.originalStart <= offset } ?: segments.first()
        val withinSeg = (offset - seg.originalStart).coerceIn(0, seg.length)
        return seg.transformedStart + withinSeg
    }

    override fun transformedToOriginal(offset: Int): Int {
        val seg = segments.lastOrNull { it.transformedStart <= offset } ?: segments.first()
        val withinSeg = (offset - seg.transformedStart).coerceIn(0, seg.length)
        return seg.originalStart + withinSeg
    }
}

/** Result of [buildHiddenComposeDecoration]: the transformed (concealed) text + its offset mapping. */
internal data class HiddenComposeResult(val transformed: AnnotatedString, val offsetMapping: OffsetMapping)

/**
 * Hide-mode analogue of [buildComposeDecoration]: content styles render as usual, [plan]'s `dim`
 * marker ranges get [markerColor], and [plan]'s `hide` marker ranges are SPLICED OUT of the
 * displayed text (Compose has no native "invisible span" like GTK's TextTag, so concealment must
 * remove the characters and report the removal via the returned [OffsetMapping]). [plan]'s ranges
 * are **UTF-8 byte** offsets (`compose_decoration_plan`'s unit, converted here alongside
 * [decorations]).
 */
internal fun buildHiddenComposeDecoration(
    text: String,
    decorations: List<MarkdownDecoration>,
    plan: ComposeMarkerPlan,
    linkColor: Color,
    markerColor: Color,
    codeBackground: Color,
): HiddenComposeResult {
    val hideRangesUtf16 = plan.hide
        .map { utf8ByteToUtf16Index(text, it.start) until utf8ByteToUtf16Index(text, it.end) }
        .filter { !it.isEmpty() }
        .sortedBy { it.first }
    val mapping = ConcealingOffsetMapping(hideRangesUtf16, text.length)

    val dimRangesUtf16 = plan.dim.map {
        utf8ByteToUtf16Index(text, it.start) to utf8ByteToUtf16Index(text, it.end)
    }

    val annotated = buildAnnotatedString {
        var cursor = 0
        for (r in hideRangesUtf16) {
            if (r.first > cursor) append(text, cursor, r.first)
            cursor = maxOf(cursor, r.last + 1)
        }
        if (cursor < text.length) append(text, cursor, text.length)

        for (d in decorations) {
            val s = utf8ByteToUtf16Index(text, d.start)
            val e = utf8ByteToUtf16Index(text, d.end)
            if (s >= e || e > text.length) continue
            val style = contentStyle(d.kind, d.level, linkColor, codeBackground)
            if (style != null) {
                addStyle(style, mapping.originalToTransformed(s), mapping.originalToTransformed(e))
            }
        }
        for ((s, e) in dimRangesUtf16) {
            if (s >= e || e > text.length) continue
            addStyle(SpanStyle(color = markerColor), mapping.originalToTransformed(s), mapping.originalToTransformed(e))
        }
        for ((s, e) in quoteLineRanges(text, decorations)) {
            val ts = mapping.originalToTransformed(s)
            val te = mapping.originalToTransformed(e)
            if (ts < te) addStyle(QuoteIndent, ts, te)
        }
    }

    return HiddenComposeResult(annotated, mapping)
}

/**
 * [VisualTransformation] that hides [plan]'s `hide` marker ranges (caret-edge reveal already
 * excludes any run the caret sits in) and dims its `dim` ranges. Non-identity offset mapping —
 * see [ConcealingOffsetMapping].
 */
class MarkdownHideVisualTransformation(
    private val decorations: List<MarkdownDecoration>,
    private val plan: ComposeMarkerPlan,
    private val linkColor: Color,
    private val markerColor: Color,
    private val codeBackground: Color,
) : VisualTransformation {
    override fun filter(text: AnnotatedString): TransformedText {
        val result = buildHiddenComposeDecoration(
            text.text, decorations, plan, linkColor, markerColor, codeBackground,
        )
        return TransformedText(result.transformed, result.offsetMapping)
    }
}
