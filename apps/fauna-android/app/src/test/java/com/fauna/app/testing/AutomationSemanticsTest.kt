package com.fauna.app.testing

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Pins the androidTest bridge's read vocabulary ([AutomationSemantics]) — the
 * part of `/element/attr`, `/element/text` and `/element/select` that does
 * not need a device. Every literal here is a cross-app contract some test
 * asserts on (`"true"`/`"false"` off an unmarked toggle, the painted token
 * winning over the native state, the Spinner rule for a select's anchor).
 */
class AutomationSemanticsTest {

    private fun node(
        resourceName: String? = "some-id",
        text: String? = null,
        stateDescription: String? = null,
        className: String? = "android.view.View",
        isCheckable: Boolean = false,
        isChecked: Boolean = false,
        isEnabled: Boolean = true,
        isClickable: Boolean = false,
    ) = NodeFacts(
        resourceName = resourceName,
        text = text,
        stateDescription = stateDescription,
        className = className,
        isCheckable = isCheckable,
        isChecked = isChecked,
        isEnabled = isEnabled,
        isClickable = isClickable,
    )

    // ── attr ────────────────────────────────────────────────────────────

    @Test
    fun unmarkedCheckboxAnswersStateAndCheckedAsTrueFalseLiterals() {
        val box = node(className = "android.widget.CheckBox", isCheckable = true, isChecked = true)
        assertEquals("true", AutomationSemantics.attrValue(box, "state"))
        assertEquals("true", AutomationSemantics.attrValue(box, "checked"))
        val off = box.copy(isChecked = false)
        assertEquals("false", AutomationSemantics.attrValue(off, "state"))
        assertEquals("false", AutomationSemantics.attrValue(off, "checked"))
    }

    @Test
    fun paintedStateDescriptionWinsOverTheNativeCheckedState() {
        // `MailExportScreen`'s mailbox boxes paint `on`/`off` themselves; the
        // native `true`/`false` must not shadow what the element chose to say.
        val box = node(stateDescription = "on", isCheckable = true, isChecked = false)
        assertEquals("on", AutomationSemantics.attrValue(box, "state"))
        assertEquals("on", AutomationSemantics.attrValue(box, "checked"))
    }

    @Test
    fun frameIsTheNodesGeometryNeverAPaintedValue() {
        // The day-timeline measurement reads `frame` as `"x,y,w,h"`; a painted
        // `stateDescription` must not answer for it.
        val block = node(stateDescription = "painted").copy(frame = AutomationSemantics.frameOf(10, 20, 300, 60))
        assertEquals("10,20,300,60", AutomationSemantics.attrValue(block, "frame"))
        assertNull(AutomationSemantics.attrValue(node(stateDescription = "painted"), "frame"))
    }

    @Test
    fun aNonToggleWithNothingPaintedHasNoState() {
        assertNull(AutomationSemantics.attrValue(node(text = "hello"), "state"))
        assertNull(AutomationSemantics.attrValue(node(text = "hello"), "class"))
    }

    @Test
    fun anyOtherNameReadsThePaintedToken() {
        // The room class / role / a copy button's `copied` — one carrier.
        val painted = node(stateDescription = "invite")
        assertEquals("invite", AutomationSemantics.attrValue(painted, "class"))
        assertEquals("invite", AutomationSemantics.attrValue(painted, "copied"))
        // An EMPTY paint is absence, not the empty string.
        assertNull(AutomationSemantics.attrValue(node(stateDescription = ""), "class"))
    }

    @Test
    fun enabledAndDisabledAreEachOthersInverse() {
        val on = node(isEnabled = true)
        assertEquals("true", AutomationSemantics.attrValue(on, "enabled"))
        assertEquals("false", AutomationSemantics.attrValue(on, "disabled"))
        val off = node(isEnabled = false)
        assertEquals("false", AutomationSemantics.attrValue(off, "enabled"))
        assertEquals("true", AutomationSemantics.attrValue(off, "disabled"))
    }

    @Test
    fun textAttrIsTheNodeText() {
        assertEquals("hello", AutomationSemantics.attrValue(node(text = "hello"), "text"))
    }

    // ── a picker's option set ───────────────────────────────────────────

    @Test
    fun optionsOffAnyNodeIsNullNeverItsPaintedToken() {
        // A TokenSelect anchor paints its SELECTED token; that is not its
        // option set (the bridge reads the opened menu for a select).
        val anchor = node(stateDescription = "automatic", className = AutomationSemantics.SPINNER_CLASS)
        assertNull(AutomationSemantics.attrValue(anchor, "options"))
        assertNull(AutomationSemantics.attrValue(node(text = "hello"), "options"))
    }

    @Test
    fun optionsListsEveryPaintedOptionInOrderAndEmptyIsNotNull() {
        // linux's `options` unit test, the same wire shape.
        assertEquals("""["Automatic","This device"]""", AutomationSemantics.optionsJson(listOf("Automatic", "This device")))
        assertEquals("[]", AutomationSemantics.optionsJson(emptyList()))
        assertEquals("""["a\"b\\c\u000a"]""", AutomationSemantics.optionsJson(listOf("a\"b\\c\n")))
    }

    @Test
    fun aMenuOptionIsItsPaintedTextElseItsTokenAndNeverTheAnchor() {
        // The human-read label, so an injectivity assertion sees what a user does.
        val item = node(resourceName = null, text = "This device", stateDescription = "this-device", isClickable = true)
        assertEquals("This device", AutomationSemantics.optionValue(item, "picker"))
        // An item painting no text still answers its token.
        assertEquals("this-device", AutomationSemantics.optionValue(item.copy(text = null), "picker"))
        // Not an option: the anchor itself, or a non-clickable label.
        assertNull(AutomationSemantics.optionValue(item.copy(resourceName = "picker"), "picker"))
        assertNull(AutomationSemantics.optionValue(item.copy(isClickable = false), "picker"))
    }

    // ── the select anchor's text ────────────────────────────────────────

    @Test
    fun aDropdownAnchorAnswersItsTokenNotItsLabel() {
        val anchor = node(
            text = "Word-pattern model",
            stateDescription = "text-model",
            className = AutomationSemantics.SPINNER_CLASS,
        )
        assertEquals("text-model", AutomationSemantics.selectedToken(anchor))
    }

    @Test
    fun aDropdownAnchorWithNoTokenKeepsItsVisibleText() {
        // A label-based picker (the admin tier select) paints no token; its
        // `get_text` stays the label, so nothing already green changes.
        val anchor = node(text = "Basic", className = AutomationSemantics.SPINNER_CLASS)
        assertNull(AutomationSemantics.selectedToken(anchor))
    }

    @Test
    fun aNonDropdownWithAStateTokenKeepsItsVisibleText() {
        // `recipient-resolve-status` carries its `state` on stateDescription
        // but is plain text: `get_text` must read the sentence, `get_attr`
        // the token.
        val status = node(text = "Resolved", stateDescription = "resolved")
        assertNull(AutomationSemantics.selectedToken(status))
        assertEquals("resolved", AutomationSemantics.attrValue(status, "state"))
    }

    // ── the option a select actuates ────────────────────────────────────

    @Test
    fun anOptionMatchesByTokenNeverByLabel() {
        val option = node(resourceName = null, text = "Word-pattern model", stateDescription = "text-model", isClickable = true)
        assertTrue(AutomationSemantics.isOptionFor(option, "text-model", "kind-select"))
        assertFalse(AutomationSemantics.isOptionFor(option, "Word-pattern model", "kind-select"))
    }

    @Test
    fun theAnchorItselfIsNeverTheOption() {
        // Re-selecting the current token: the anchor carries it too, and
        // tapping the anchor again would close the menu it just opened.
        val anchor = node(resourceName = "kind-select", stateDescription = "list", isClickable = true)
        assertFalse(AutomationSemantics.isOptionFor(anchor, "list", "kind-select"))
    }

    @Test
    fun aNonClickableNodeCarryingTheTokenIsNotAnOption() {
        val label = node(resourceName = null, stateDescription = "list", isClickable = false)
        assertFalse(AutomationSemantics.isOptionFor(label, "list", "kind-select"))
    }
}
