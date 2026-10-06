package com.fauna.app.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Verifies the shell-logging shim ([ShellLog]) and the category-1 display funnel
 * ([AppMessages]) stay safe when the native `fauna-log` `.so` is absent — the
 * unit-test JVM has no native lib, so `com.fauna.ffi.logMessage` throws an
 * `UnsatisfiedLinkError` that the shim must swallow. This is the regression guard
 * for "logging never crashes a producer or the banner" (observability.md § The
 * emit API; the guard mirrors `FaunaApp`'s install try/catch).
 */
class ShellLogTest {

    @Test
    fun shellLogNeverThrowsWithoutNativeLib() {
        // No fauna-log .so on the unit-test JVM → logMessage throws; the shim's
        // catch(Throwable) must absorb it so the caller's control flow survives.
        ShellLog.e("test", "an error")
        ShellLog.w("test", "a warning")
        ShellLog.i("test", "info")
        ShellLog.d("test", "debug")
        // Reaching this line means no Throwable escaped any of the four calls.
    }

    @Test
    fun appMessagesShowSetsStateAndDoesNotThrow() {
        val m = AppMessages()
        m.showError("boom")
        assertEquals("boom", m.error.value)
        m.showWarning("careful")
        assertEquals("careful", m.warning.value)
        m.showInfo("saved")
        assertEquals("saved", m.info.value)
    }

    @Test
    fun appMessagesClearResetsState() {
        val m = AppMessages()
        m.showError("boom")
        m.showWarning("careful")
        m.showInfo("saved")
        m.clear()
        assertNull(m.error.value)
        assertNull(m.warning.value)
        assertNull(m.info.value)
    }

    @Test
    fun appMessagesShowNullClearsWithoutLogging() {
        val m = AppMessages()
        m.showError("boom")
        m.showError(null)
        assertNull(m.error.value)
    }
}
