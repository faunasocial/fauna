package com.fauna.app.testing

/**
 * The facts the androidTest bridge (`androidTest/.../bridge/ElementOps.kt`)
 * reads off one accessibility node, lifted off `AccessibilityNodeInfo` so the
 * read rules in [AutomationSemantics] are pure and unit-pinned
 * (`AutomationSemanticsTest`) — the bridge itself only ever runs on a device,
 * and the vocabulary is exactly the part three other apps got wrong on their
 * first pass.
 *
 * Lives in the debug source set beside [TestAgent]: the automation surface is
 * compiled out of release artifacts (e2e convention 15), and the debug variant
 * is what `androidTest` and `testDebugUnitTest` both see.
 */
data class NodeFacts(
    /** `viewIdResourceName` — a Compose `testTag` under `testTagsAsResourceId`. */
    val resourceName: String?,
    val text: String?,
    /** Compose `Modifier.semantics { stateDescription = … }` — this app's one
     *  carrier for a string state attribute (`recipient-resolve-status`'s
     *  `state`, the room class token, a [com.fauna.app.ui.components.TokenSelect]'s
     *  selected token). */
    val stateDescription: String?,
    /** The accessibility class — `android.widget.Spinner` for a `Role.DropdownList`
     *  anchor, `android.widget.CheckBox` for a `Role.Checkbox`, … */
    val className: String?,
    val isCheckable: Boolean,
    val isChecked: Boolean,
    val isEnabled: Boolean,
    val isClickable: Boolean,
    /** `AccessibilityNodeInfo.isEditable` — a text field a user can type into;
     *  a read-only field (an `ExposedDropdownMenuBox` anchor) is not. */
    val isEditable: Boolean = false,
    /** The node's on-screen rect as `"x,y,w,h"` ([AutomationSemantics.frameOf]) —
     *  the `frame` attribute. Accessibility bounds are CLIPPED to the visible
     *  part, as windows' UIA rects are; `null` when not read. */
    val frame: String? = null,
)

/**
 * The bridge's read rules over [NodeFacts] — the android twin of linux's
 * `automation/agent.rs::attr` vocabulary, so one cross-app
 * `driver.get_attr(id, name)` / `get_text(select)` / `select(id, token)`
 * reads the same thing on both.
 */
object AutomationSemantics {
    /** The accessibility class Compose reports for a `Role.DropdownList` node. */
    const val SPINNER_CLASS = "android.widget.Spinner"

    /** The accessibility class Compose reports for a text field. */
    const val EDIT_TEXT_CLASS = "android.widget.EditText"

    /**
     * `GET /element/attr?attr=[name]`. The precedence mirrors linux's agent:
     * an EXPLICIT painted value (here `stateDescription`; there a
     * `test-attr-{name}-{value}` marker class) always wins, and only an
     * unmarked toggle falls back to its live checked state spelled
     * `"true"`/`"false"` — never `"on"`/`"off"`, which an element wanting
     * those literals paints itself (`MailExportScreen`'s mailbox boxes do).
     * `null` = the element carries no such attribute, which the driver
     * collapses with "element missing" (`drivers/http_bridge.py::get_attr`).
     */
    fun attrValue(node: NodeFacts, name: String): String? = when (name) {
        "enabled" -> flag(node.isEnabled)
        "disabled" -> flag(!node.isEnabled)
        "text" -> node.text
        // `state` / `checked`: the two names the cross-app tests read a toggle
        // by (tui publishes `checked` for every checkbox; `state` is the older
        // idiom) — same answer, same precedence.
        "state", "checked" -> paintedAttr(node, name)
            ?: if (node.isCheckable) flag(node.isChecked) else null
        // `frame`: the node's own screen geometry, never a painted value —
        // what `actions/events.py`'s day-timeline measurement reads (windows
        // publishes the same `"x,y,w,h"` shape off UIA).
        "frame" -> node.frame
        // `options`: a picker's option set lives in its OPENED menu, never on
        // the anchor node, so the bridge answers a select's anchor itself
        // ([optionValue] over the menu, [optionsJson]); every other node
        // carries no option set — an explicit `null`, never the painted
        // `stateDescription` (a `TokenSelect` anchor's is its SELECTED token).
        "options" -> null
        // Any other name — `class`, `role`, `copied`, `kind`, … — is the one
        // string attribute a node can publish, on its `stateDescription`; a
        // node publishing several packs them there ([packedValue]).
        else -> paintedAttr(node, name)
    }

    /**
     * The per-key reading of a `stateDescription` that carries SEVERAL
     * attributes — the reader half of
     * [com.fauna.app.ui.util.AutomationAttributePack], the same grammar
     * windows' bridge holds (`tests/e2e-unified/flaui-bridge/PackedAttr.cs`):
     * two or more `key=value` pairs joined by `;`, each key lowercase kebab
     * (`[a-z][a-z0-9-]*`), a value running from its key's first `=` to the
     * next `;`, `%` written `%25` and `;` written `%3B`.
     *
     * Returns `null` when [painted] is NOT a pack — the caller keeps it
     * verbatim, since most nodes publish one bare value (a token, an id that
     * may end in base64 `=`); otherwise a holder whose `value` is [key]'s
     * unescaped value, or `null` when the pack names no such key (the
     * attribute is not set).
     */
    fun packedValue(painted: String?, key: String): Packed? {
        if (painted.isNullOrEmpty()) return null
        val pairs = painted.split(';')
        if (pairs.size < 2) return null
        var found: String? = null
        for (pair in pairs) {
            val eq = pair.indexOf('=')
            if (eq <= 0 || !isKey(pair.substring(0, eq))) return null
            if (found == null && pair.substring(0, eq) == key) found = unescape(pair.substring(eq + 1))
        }
        return Packed(found)
    }

    /** [packedValue]'s answer for a pack: [value] is `null` when the key is absent. */
    data class Packed(val value: String?)

    /**
     * What `GET /element/text` answers for a select's anchor: the selected
     * TOKEN when the node is a dropdown (the widget-type-aware read the raw-
     * value select contract needs — `personalization-trained-factor-publish-
     * kind-select` round-trips `list`/`text-model`, never the label), `null`
     * for everything else so the caller keeps the node's visible text.
     */
    fun selectedToken(node: NodeFacts): String? =
        if (node.className == SPINNER_CLASS) painted(node) else null

    /**
     * Whether [node] anchors a menu the `options` read may open: a
     * [com.fauna.app.ui.components.TokenSelect] anchor ([SPINNER_CLASS]), or
     * the READ-ONLY text field an `ExposedDropdownMenuBox` anchors with
     * `menuAnchor()` — the admin guardian, web-apex and DNS pickers, which
     * paint the option's label in the field and keep no token. An editable
     * text field is never a picker: its `options` stay `null`.
     */
    fun isMenuAnchor(node: NodeFacts): Boolean =
        node.className == SPINNER_CLASS || (node.className == EDIT_TEXT_CLASS && !node.isEditable)

    /**
     * Whether [node] is the menu option `select(anchorId, value)` should
     * actuate: a clickable node carrying [value] as its `stateDescription`
     * that is NOT the anchor itself — the anchor also carries the currently
     * selected token, and re-selecting it must not re-tap the anchor (which
     * would close the menu it just opened).
     */
    fun isOptionFor(node: NodeFacts, value: String, anchorId: String): Boolean =
        node.isClickable && node.resourceName != anchorId && node.stateDescription == value

    /**
     * One node of an opened select menu, as the `options` attr lists it: a
     * clickable item's PAINTED text — the contract is option TEXTS
     * (`drivers/base.py::option_texts`; the admin pickers' injectivity
     * assertion is over what the human reads, which a token would hide) —
     * falling back to its `stateDescription` token only for an item that
     * paints no text; `null` for anything that is not an option (a
     * non-clickable node, the anchor itself).
     */
    fun optionValue(node: NodeFacts, anchorId: String): String? =
        if (!node.isClickable || node.resourceName == anchorId) null
        else node.text?.takeIf { it.isNotEmpty() } ?: painted(node)

    /**
     * The `options` attr's wire value: every option the picker paints, in
     * paint order, JSON-encoded — the contract `drivers/base.py::option_texts`
     * reads and tui, web, apple, linux and windows serve. An empty list
     * encodes as `"[]"`: zero options is a distinct fact from a non-picker's
     * `null`. Hand-encoded so the rule stays JVM-pure (`org.json` is an
     * android.jar stub under a plain unit test).
     */
    fun optionsJson(values: List<String>): String =
        values.joinToString(",", "[", "]") { jsonString(it) }

    private fun jsonString(s: String): String = buildString {
        append('"')
        for (c in s) {
            when {
                c == '"' -> append("\\\"")
                c == '\\' -> append("\\\\")
                c < ' ' -> append("\\u%04x".format(c.code))
                else -> append(c)
            }
        }
        append('"')
    }

    /** `"x,y,w,h"` — the `frame` wire shape every bridge answers. */
    fun frameOf(x: Int, y: Int, w: Int, h: Int): String = "$x,$y,$w,$h"

    private fun painted(node: NodeFacts): String? =
        node.stateDescription?.takeIf { it.isNotEmpty() }

    /** [name]'s painted value: its key's value out of a pack, else the bare paint. */
    private fun paintedAttr(node: NodeFacts, name: String): String? {
        val bare = painted(node)
        val packed = packedValue(bare, name) ?: return bare
        return packed.value?.takeIf { it.isNotEmpty() }
    }

    private fun isKey(key: String): Boolean =
        key.isNotEmpty() && key[0] in 'a'..'z' && key.all { it in 'a'..'z' || it in '0'..'9' || it == '-' }

    // `%3B` before `%25`: the writer escaped `%` first, so a literal "%3B" in a
    // value was written "%253B" — undoing `%25` first would turn it into `;`.
    private fun unescape(value: String): String = value.replace("%3B", ";").replace("%25", "%")

    private fun flag(on: Boolean): String = if (on) "true" else "false"
}
