package com.fauna.app.bridge

import android.app.UiAutomation
import android.graphics.Rect
import android.os.SystemClock
import android.view.accessibility.AccessibilityNodeInfo
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.UiObject2
import com.fauna.app.testing.AutomationSemantics
import com.fauna.automation.ScopeStep
import com.fauna.automation.UiElements
import com.fauna.app.testing.NodeFacts

/**
 * The bridge's element operations, over two views of the same screen:
 *
 * - **UiAutomator** (`UiObject2`) for finding, counting and actuating —
 *   [UiElements], the half this bridge shares with the out-of-process store
 *   driver (`automation-shared/`), which every plain find/actuate route here
 *   forwards to;
 * - **the raw accessibility tree** (`AccessibilityNodeInfo`, through the
 *   instrumentation's [UiAutomation]) for the one fact `UiObject2` does not
 *   expose: `stateDescription`, this app's carrier for every string state
 *   attribute and for a [com.fauna.app.ui.components.TokenSelect]'s token.
 *   The rules applied over it are pure and unit-pinned
 *   ([AutomationSemantics], `AutomationSemanticsTest`); only the tree walk
 *   lives here.
 *
 * ⚠ Compile-verified only; the tree walk's device behaviour (which
 * window is active while a `DropdownMenu` popup is open, whether a clipped
 * node's bounds still match) is unwitnessed until android has an e2e run
 * venue.
 */
class ElementOps(
    private val device: UiDevice,
    private val automation: () -> UiAutomation,
) {

    private val elements = UiElements(device)

    private fun findAll(elementId: String, scope: List<ScopeStep> = emptyList()): List<UiObject2> =
        elements.findAll(elementId, scope)

    private fun one(elementId: String, index: Int, scope: List<ScopeStep>): UiObject2 =
        elements.one(elementId, index, scope)

    // ── actions (the shared half) ───────────────────────────────────────

    fun click(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()) =
        elements.click(elementId, index, scope)

    fun doubleClick(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()) =
        elements.doubleClick(elementId, index, scope)

    fun type(elementId: String, text: String, index: Int = 0, scope: List<ScopeStep> = emptyList()) =
        elements.type(elementId, text, index, scope)

    fun clear(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()) =
        elements.clear(elementId, index, scope)

    fun pressKey(elementId: String, key: String, index: Int = 0, scope: List<ScopeStep> = emptyList()) =
        elements.pressKey(elementId, key, index, scope)

    fun pressBack() = elements.pressBack()

    // ── actions over the raw tree ───────────────────────────────────────

    /**
     * Actuate a select (ui.yaml `select`): click the anchor [elementId] to
     * open its menu, then click the option whose VALUE is [value] — a
     * [com.fauna.app.ui.components.TokenSelect] item carrying the token on
     * `stateDescription` — falling back to the option whose visible text is
     * [value], which is how a label-based picker (the admin tier select the
     * shared `actions/admin.py::_pick_tier` drives) has always been found.
     * Mirrors the cross-app `driver.select` contract.
     */
    fun select(elementId: String, value: String, index: Int = 0, scope: List<ScopeStep> = emptyList()) {
        one(elementId, index, scope).click() // expand the dropdown
        val deadline = SystemClock.uptimeMillis() + SELECT_OPTION_WAIT_MS
        while (true) {
            optionNode(elementId, value)?.let { option ->
                if (!option.performAction(AccessibilityNodeInfo.ACTION_CLICK)) {
                    val bounds = boundsOf(option)
                    device.click(bounds.centerX(), bounds.centerY())
                }
                return
            }
            device.findObject(By.text(value))?.let {
                it.click()
                return
            }
            if (SystemClock.uptimeMillis() >= deadline) throw SelectOptionNotOffered(elementId, value)
            // A bounded poll for the popup to paint — `Until.findObject` cannot
            // search `stateDescription`, so this is the same wait it would do,
            // spelled out.
            SystemClock.sleep(SELECT_OPTION_POLL_MS)
        }
    }

    /**
     * `POST /element/scroll-into-view` — bring the [index]th [elementId] wholly
     * into view with the accessibility `ACTION_SHOW_ON_SCREEN`, which Compose
     * answers with a `bringIntoView` of the node's whole rect through every
     * scrollable ancestor. Read off the raw tree, not `UiObject2`: a node
     * scrolled out of a `verticalScroll` is still in the accessibility tree
     * (`isVisibleToUser` false) but UiAutomator does not report it. `null` on
     * success, else the reason (`found: false` on the wire).
     */
    fun scrollIntoView(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()): String? {
        val all = a11yFind(elementId, scope)
        val node = all.getOrNull(index) ?: return "not found ($index of ${all.size} matches)"
        if (!node.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SHOW_ON_SCREEN.id) &&
            !node.isVisibleToUser
        ) {
            return "ACTION_SHOW_ON_SCREEN refused and the element is off screen"
        }
        device.waitForIdle()
        return null
    }

    // ── reads ───────────────────────────────────────────────────────────

    /**
     * `GET /element/text`. A select's anchor answers its selected TOKEN
     * ([AutomationSemantics.selectedToken] — the widget-type-aware read the
     * raw-value select contract needs); every other node answers its visible
     * text, as before.
     */
    fun getText(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()): String {
        val target = one(elementId, index, scope)
        if (target.className == AutomationSemantics.SPINNER_CLASS) {
            twinOf(target, elementId, index, scope)
                ?.let { AutomationSemantics.selectedToken(factsOf(it)) }
                ?.let { return it }
        }
        return target.text ?: ""
    }

    /**
     * `GET /element/attr` — [AutomationSemantics.attrValue] over the node's
     * facts. `null` when the element carries no such attribute (the driver
     * reads that as `None`); a missing element is [ElementNotFound].
     */
    fun getAttr(elementId: String, name: String, index: Int = 0, scope: List<ScopeStep> = emptyList()): String? {
        val target = one(elementId, index, scope)
        val twin = twinOf(target, elementId, index, scope)
        val facts = twin?.let(::factsOf) ?: factsOf(target)
        if (name == "options" && twin != null && AutomationSemantics.isMenuAnchor(facts)) {
            return AutomationSemantics.optionsJson(paintedOptions(target, twin, elementId))
        }
        return AutomationSemantics.attrValue(facts, name)
    }

    /**
     * Every option a select's menu paints, in paint order — the `options`
     * attr. A Compose menu exists only while open, so the read opens it (the
     * same tap [select] makes), collects [AutomationSemantics.optionValue]
     * over the popup's window — any of this app's windows but the anchor's,
     * so neither the page's other select anchors nor the system UI's windows
     * are mistaken for options — and closes it
     * again with back, which a popup consumes before the screen does. Only
     * a popup that actually painted is closed: an empty list never sends a
     * back to the page.
     */
    private fun paintedOptions(target: UiObject2, anchor: AccessibilityNodeInfo, anchorId: String): List<String> {
        target.click()
        val deadline = SystemClock.uptimeMillis() + SELECT_OPTION_WAIT_MS
        while (true) {
            val values = ArrayList<String>()
            for (root in roots()) {
                if (root.windowId == anchor.windowId || root.packageName != anchor.packageName) continue
                walk(root) { node ->
                    if (node.isVisibleToUser) {
                        AutomationSemantics.optionValue(factsOf(node), anchorId)?.let(values::add)
                    }
                }
            }
            if (values.isNotEmpty()) {
                device.pressBack()
                device.waitForIdle()
                return values
            }
            if (SystemClock.uptimeMillis() >= deadline) return values
            SystemClock.sleep(SELECT_OPTION_POLL_MS)
        }
    }

    fun isEnabled(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()): Boolean =
        elements.isEnabled(elementId, index, scope)

    fun isVisible(elementId: String, scope: List<ScopeStep> = emptyList()): Boolean =
        elements.isVisible(elementId, scope)

    fun count(elementId: String, scope: List<ScopeStep> = emptyList()): Int =
        elements.count(elementId, scope)

    fun screenshot(name: String): String = elements.screenshot(name)

    // ── the raw accessibility tree ──────────────────────────────────────

    /** Every window's root, the active window's first — UiAutomator's own order. */
    private fun roots(): List<AccessibilityNodeInfo> {
        val auto = automation()
        val out = ArrayList<AccessibilityNodeInfo>()
        auto.rootInActiveWindow?.let { out.add(it) }
        try {
            for (window in auto.windows) {
                val root = window.root ?: continue
                if (out.none { it.windowId == root.windowId }) out.add(root)
            }
        } catch (_: Exception) {
            // Multi-window retrieval needs FLAG_RETRIEVE_INTERACTIVE_WINDOWS,
            // which UiDevice sets; the active window alone is still a valid
            // (narrower) answer.
        }
        return out
    }

    private fun walk(node: AccessibilityNodeInfo, visit: (AccessibilityNodeInfo) -> Unit) {
        visit(node)
        for (i in 0 until node.childCount) {
            node.getChild(i)?.let { walk(it, visit) }
        }
    }

    private fun matchesId(node: AccessibilityNodeInfo, id: String): Boolean =
        node.viewIdResourceName == id || node.contentDescription?.toString() == id

    /** Document-order matches of [id] below every subtree in [subtrees]. */
    private fun collect(subtrees: List<AccessibilityNodeInfo>, id: String): List<AccessibilityNodeInfo> {
        val out = ArrayList<AccessibilityNodeInfo>()
        for (root in subtrees) walk(root) { if (matchesId(it, id)) out.add(it) }
        return out
    }

    private fun a11yFind(id: String, scope: List<ScopeStep>): List<AccessibilityNodeInfo> {
        var subtrees: List<AccessibilityNodeInfo> = roots()
        for (step in scope) {
            val match = collect(subtrees, step.id).getOrNull(step.index) ?: return emptyList()
            subtrees = listOf(match)
        }
        return collect(subtrees, id)
    }

    /**
     * The accessibility node behind a `UiObject2`: the same-id node whose
     * screen bounds are the object's, or — when that is not unique (a
     * clipped node's visible bounds are a subset of its own) — the same-id
     * node at the object's index, both walks being document order over the
     * same tree.
     */
    private fun twinOf(target: UiObject2, id: String, index: Int, scope: List<ScopeStep>): AccessibilityNodeInfo? {
        val candidates = a11yFind(id, scope)
        val bounds = target.visibleBounds
        val exact = candidates.filter { boundsOf(it) == bounds }
        if (exact.size == 1) return exact[0]
        return candidates.getOrNull(index)
    }

    /** The first visible menu option carrying [value] as its token, if any. */
    private fun optionNode(anchorId: String, value: String): AccessibilityNodeInfo? {
        var found: AccessibilityNodeInfo? = null
        for (root in roots()) {
            walk(root) { node ->
                if (found == null && node.isVisibleToUser &&
                    AutomationSemantics.isOptionFor(factsOf(node), value, anchorId)
                ) {
                    found = node
                }
            }
            if (found != null) break
        }
        return found
    }

    private fun boundsOf(node: AccessibilityNodeInfo): Rect = Rect().also { node.getBoundsInScreen(it) }

    private fun frameOf(r: Rect): String = AutomationSemantics.frameOf(r.left, r.top, r.width(), r.height())

    private fun factsOf(node: AccessibilityNodeInfo) = NodeFacts(
        frame = frameOf(boundsOf(node)),
        resourceName = node.viewIdResourceName,
        text = node.text?.toString(),
        stateDescription = AccessibilityNodeInfoCompat.wrap(node).stateDescription?.toString(),
        className = node.className?.toString(),
        isCheckable = node.isCheckable,
        isChecked = node.isChecked,
        isEnabled = node.isEnabled,
        isClickable = node.isClickable,
        isEditable = node.isEditable,
        isSelected = node.isSelected,
    )

    /** The facts `UiObject2` exposes on its own — everything but `stateDescription` and editability. */
    private fun factsOf(obj: UiObject2) = NodeFacts(
        frame = frameOf(obj.visibleBounds),
        resourceName = obj.resourceName,
        text = obj.text,
        stateDescription = null,
        className = obj.className,
        isCheckable = obj.isCheckable,
        isChecked = obj.isChecked,
        isEnabled = obj.isEnabled,
        isClickable = obj.isClickable,
    )

    private companion object {
        const val SELECT_OPTION_WAIT_MS = 2000L
        const val SELECT_OPTION_POLL_MS = 50L
    }
}

/**
 * The picker `id` rendered, but the frame never painted `value` among its
 * options — a distinct refusal from [ElementNotFound] (the picker itself
 * missing). Mapped to HTTP 409 in [BridgeHttpServer], matching every other
 * app's `select`-refusal shape (`tests/e2e-unified/drivers/http_bridge.py`'s
 * `SelectOptionNotOffered` — convention 11's twin rule,
 * `docs/goal/architecture/e2e-conventions.md`). Previously rode
 * [ElementNotFound] → 404, which read as "picker not rendered yet" and burned
 * the driver's scroll-retry loop three times before failing for the wrong
 * reason.
 */
class SelectOptionNotOffered(id: String, value: String) :
    Exception("Picker '$id' does not offer option '$value'")
