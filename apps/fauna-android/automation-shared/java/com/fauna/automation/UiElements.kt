package com.fauna.automation

import android.view.KeyEvent
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.UiObject2
import org.json.JSONArray
import org.json.JSONObject

/**
 * The find/actuate half of an Android automation bridge, shared by both bridges
 * that drive the app through UiAutomator: the harness bridge in the app's own
 * test APK (`app/src/androidTest/.../bridge/`, whose `ElementOps` adds the
 * raw-accessibility-tree reads on top of this), and the out-of-process store
 * driver (`store-driver/`), which reaches a Play-signed install through
 * `UiAutomation` alone (`docs/goal/architecture/feature-catalog.md`
 * § Implementation status today, how the Android store install is driven).
 *
 * Nothing here needs the app's UID or the app's code: the matcher is the
 * accessibility tree any accessibility service reads — `By.res(id)`, a Compose
 * `testTag` the app exposes under `testTagsAsResourceId` (`MainActivity`, main
 * source set, kept in release), with `By.desc(id)` as the fallback — so this
 * file compiles into a test APK signed with any key.
 */
class UiElements(private val device: UiDevice) {

    // ── finding ─────────────────────────────────────────────────────────

    private fun findIn(roots: List<UiObject2>?, id: String): List<UiObject2> {
        if (roots == null) {
            val byRes = device.findObjects(By.res(id))
            if (byRes.isNotEmpty()) return byRes
            return device.findObjects(By.desc(id))
        }
        val byRes = roots.flatMap { it.findObjects(By.res(id)) }
        if (byRes.isNotEmpty()) return byRes
        return roots.flatMap { it.findObjects(By.desc(id)) }
    }

    /**
     * Every match of [elementId] below [scope] (document order). A scope step
     * that resolves to nothing is the element-not-found refusal for THAT step,
     * so a wrong container reads as its own 404 rather than as a plausible
     * value from elsewhere on the page.
     */
    fun findAll(elementId: String, scope: List<ScopeStep> = emptyList()): List<UiObject2> {
        var roots: List<UiObject2>? = null
        for (step in scope) {
            val matches = findIn(roots, step.id)
            if (step.index >= matches.size) throw ElementNotFound(step.id, step.index, matches.size)
            roots = listOf(matches[step.index])
        }
        return findIn(roots, elementId)
    }

    /** The [index]th match of [elementId] below [scope], or [ElementNotFound]. */
    fun one(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()): UiObject2 {
        val all = findAll(elementId, scope)
        if (index >= all.size) throw ElementNotFound(elementId, index, all.size)
        return all[index]
    }

    // ── actions ─────────────────────────────────────────────────────────

    fun click(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()) {
        one(elementId, index, scope).click()
    }

    /**
     * `double_click` (`POST /element/double_click`, `drivers/http_bridge.py::double_click`):
     * two taps at the element's centre inside the platform double-tap window, so a
     * `combinedClickable(onDoubleClick = …)` target sees one double-tap, not two
     * single taps.
     */
    fun doubleClick(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()) {
        val bounds = one(elementId, index, scope).visibleBounds
        device.click(bounds.centerX(), bounds.centerY())
        device.click(bounds.centerX(), bounds.centerY())
    }

    fun type(elementId: String, text: String, index: Int = 0, scope: List<ScopeStep> = emptyList()) {
        one(elementId, index, scope).text = text
    }

    fun clear(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()) {
        one(elementId, index, scope).clear()
    }

    /**
     * `press_key` (`POST /element/key`): one hardware key event delivered to [elementId],
     * focusing it first only when it is not already focused — a tap moves a text field's
     * caret to the tap point, and a caret-moving key must start where the caret already
     * is. The key names are the cross-app driver's (`ArrowLeft`, `End`, `Enter`, …; see
     * [keyCodeFor]); a name with no mapping is refused, never dropped.
     */
    fun pressKey(elementId: String, key: String, index: Int = 0, scope: List<ScopeStep> = emptyList()) {
        val code = keyCodeFor(key) ?: throw UnsupportedKey(key)
        val target = one(elementId, index, scope)
        if (!target.isFocused) target.click()
        device.pressKeyCode(code)
    }

    /**
     * `POST /device/back` — the system back, the same key event the phone's back
     * gesture/button delivers, so whichever `BackHandler` the current screen
     * registered handles it exactly as for a user. Not element-targeted: back
     * goes to the focused window, never to a field.
     */
    fun pressBack() {
        device.pressBack()
    }

    // ── reads ───────────────────────────────────────────────────────────

    /** The node's visible text, `""` when it has none. */
    fun text(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()): String =
        one(elementId, index, scope).text ?: ""

    /** `GET /element/enabled` — the cross-app `is_enabled` read. */
    fun isEnabled(elementId: String, index: Int = 0, scope: List<ScopeStep> = emptyList()): Boolean =
        one(elementId, index, scope).isEnabled

    fun isVisible(elementId: String, scope: List<ScopeStep> = emptyList()): Boolean =
        findAll(elementId, scope).isNotEmpty()

    fun count(elementId: String, scope: List<ScopeStep> = emptyList()): Int =
        findAll(elementId, scope).size

    fun screenshot(name: String): String {
        val file = java.io.File("/data/local/tmp/screenshots/$name.png")
        file.parentFile?.mkdirs()
        device.takeScreenshot(file)
        return file.absolutePath
    }
}

/**
 * One step of a scoped query — `[{"id": …, "index": …}]` on the wire
 * (`tests/e2e-unified/drivers/scope.py::scope_to_wire`): each step names a
 * container found anywhere below the previous step's subtree, and `index`
 * picks among those instances in document order.
 */
data class ScopeStep(val id: String, val index: Int)

/** The `scope` of a POST body — an array of `{"id", "index"}` steps, or none. */
fun scopeOf(body: JSONObject): List<ScopeStep> =
    body.optJSONArray("scope")?.let(::scopeSteps) ?: emptyList()

/** The `scope` of a GET query — the same array, JSON-encoded as a string, or none. */
fun parseScope(raw: String?): List<ScopeStep> {
    if (raw.isNullOrEmpty()) return emptyList()
    return scopeSteps(JSONArray(raw))
}

private fun scopeSteps(steps: JSONArray): List<ScopeStep> =
    (0 until steps.length()).map { i ->
        val step = steps.getJSONObject(i)
        ScopeStep(step.getString("id"), step.optInt("index", 0))
    }

/** The cross-app `press_key` names a bridge can deliver, as Android key codes. */
fun keyCodeFor(key: String): Int? = when (key) {
    "ArrowLeft" -> KeyEvent.KEYCODE_DPAD_LEFT
    "ArrowRight" -> KeyEvent.KEYCODE_DPAD_RIGHT
    "ArrowUp" -> KeyEvent.KEYCODE_DPAD_UP
    "ArrowDown" -> KeyEvent.KEYCODE_DPAD_DOWN
    "Home" -> KeyEvent.KEYCODE_MOVE_HOME
    "End" -> KeyEvent.KEYCODE_MOVE_END
    "Enter" -> KeyEvent.KEYCODE_ENTER
    "Escape" -> KeyEvent.KEYCODE_ESCAPE
    "Tab" -> KeyEvent.KEYCODE_TAB
    "Backspace" -> KeyEvent.KEYCODE_DEL
    "Delete" -> KeyEvent.KEYCODE_FORWARD_DEL
    else -> null
}

/** A `press_key` name [keyCodeFor] has no key code for — HTTP 400 on either bridge. */
class UnsupportedKey(key: String) : Exception("Key '$key' has no Android key code on this bridge")

/** No [index]th match of an element id — HTTP 404 on either bridge. */
class ElementNotFound(id: String, index: Int, found: Int) :
    Exception("Element '$id' index $index not found (found $found)")
