package com.fauna.app.ui.components

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The two-click confirm rule (`common.md` § Two-click confirm): arm, then perform. */
class TwoClickArmTest {
    @Test
    fun firstPressArmsWithoutPerforming() {
        val arm = TwoClickArm()
        var performed = 0
        arm.press { performed++ }
        assertTrue(arm.armed)
        assertEquals(0, performed)
    }

    @Test
    fun secondPressPerformsOnceAndDisarms() {
        val arm = TwoClickArm()
        var performed = 0
        arm.press { performed++ }
        arm.press { performed++ }
        assertFalse(arm.armed)
        assertEquals(1, performed)
    }

    @Test
    fun disarmDropsTheArmSoTheNextPressArmsAgain() {
        val arm = TwoClickArm()
        var performed = 0
        arm.press { performed++ }
        arm.disarm()
        arm.press { performed++ }
        assertTrue(arm.armed)
        assertEquals(0, performed)
    }

    @Test
    fun disarmTimeoutIsTheRatifiedFourSeconds() {
        assertEquals(4_000L, TwoClickArm.DISARM_AFTER_MS)
    }
}
