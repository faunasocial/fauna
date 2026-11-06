package com.fauna.app.ui.viewmodel

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Pins the admin gate's reconnect re-read rule (`transport.md` `admin_gate`):
 * a failed read keeps a confirmed admin's entry, and a confirming re-read does
 * not re-fire the once-per-session admin effects (the re-auth auto-default).
 */
class SettingsAdminGateVMTest {

    @Test
    fun failedReReadKeepsConfirmedAdminAndFiresNoEffect() {
        assertEquals(true to false, applyAdminGateRead(prev = true, read = null))
    }

    @Test
    fun failedReadLeavesNonAdminHidden() {
        assertEquals(false to false, applyAdminGateRead(prev = false, read = null))
    }

    @Test
    fun confirmingReReadDoesNotRefireAdminEffects() {
        assertEquals(true to false, applyAdminGateRead(prev = true, read = true))
    }

    @Test
    fun falseToTrueTransitionFiresAdminEffectsOnce() {
        assertEquals(true to true, applyAdminGateRead(prev = false, read = true))
    }

    @Test
    fun successfulNotAdminAnswerClosesTheGate() {
        assertEquals(false to false, applyAdminGateRead(prev = true, read = false))
    }
}
