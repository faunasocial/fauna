package com.fauna.app.ui.components

import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * The timed half of the two-click confirm rule (`common.md` § Two-click
 * confirm) that [TwoClickArmTest]'s plain-JVM cases cannot reach: an arm that
 * sees no second press within [TwoClickArm.DISARM_AFTER_MS] drops back to the
 * resting label, so the next press arms again instead of performing. Drives
 * [rememberTwoClickArm] on a paused Compose clock.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TwoClickArmComposeTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private var performed = 0

    private fun setButton() {
        composeTestRule.setContent {
            val arm = rememberTwoClickArm()
            Button(onClick = { arm.press { performed++ } }, modifier = Modifier.testTag("btn")) {
                Text(if (arm.armed) "Confirm?" else "Delete")
            }
        }
    }

    @Test
    fun armDisarmsAfterFourSecondsWithoutPerforming() {
        composeTestRule.mainClock.autoAdvance = false
        setButton()
        val btn = composeTestRule.onNodeWithTag("btn")

        btn.performClick()
        composeTestRule.mainClock.advanceTimeByFrame()
        btn.assertTextEquals("Confirm?")

        composeTestRule.mainClock.advanceTimeBy(TwoClickArm.DISARM_AFTER_MS - 100)
        btn.assertTextEquals("Confirm?")

        composeTestRule.mainClock.advanceTimeBy(200)
        btn.assertTextEquals("Delete")

        // The disarmed control's next press arms again — it never performs.
        btn.performClick()
        composeTestRule.mainClock.advanceTimeByFrame()
        btn.assertTextEquals("Confirm?")
        assertEquals(0, performed)
    }

    @Test
    fun secondPressInsideTheWindowPerformsOnce() {
        composeTestRule.mainClock.autoAdvance = false
        setButton()
        val btn = composeTestRule.onNodeWithTag("btn")

        btn.performClick()
        composeTestRule.mainClock.advanceTimeBy(TwoClickArm.DISARM_AFTER_MS - 100)
        btn.performClick()
        composeTestRule.mainClock.advanceTimeByFrame()

        assertEquals(1, performed)
        btn.assertTextEquals("Delete")
    }
}
