package com.fauna.app.ui.screen.onboarding

import com.fauna.app.core.LogicalSecretStore
import com.fauna.app.core.MemoryBackend
import com.fauna.app.core.OnboardingHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.onboarding.OnboardingMachine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * `IdentityImportVM`'s twin of [IdentityCreatedVMTest] —
 * the same confirm-identity moment on the import leg, over a real registry.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class IdentityImportVMTest {

    private val backend = MemoryBackend()
    private val registry = FfiAccountRegistry(LogicalSecretStore(backend))

    private fun makeVm(appendMode: Boolean): IdentityImportVM {
        val host = mock(OnboardingHost::class.java)
        val machine = mock(OnboardingMachine::class.java)
        whenever(host.machine).thenReturn(machine)
        whenever(host.appendMode).thenReturn(appendMode)
        whenever(machine.confirmImportedIdentity(SECRET)).thenReturn(SECRET)
        return IdentityImportVM(host, registry)
    }

    @Test
    fun normalOnboard_registersAndActivatesTheIdentity() {
        assertTrue(makeVm(appendMode = false).confirm(SECRET))

        val active = registry.active()
        assertNotNull("the first-run confirm activates the imported identity", active)
        assertEquals(SECRET, registry.sessionMaterial(active!!)?.secretHex)
    }

    /** An abandoned append registers nothing: no account, nothing active. */
    @Test
    fun appendMode_registersNothing() {
        assertTrue(makeVm(appendMode = true).confirm(SECRET))

        assertTrue("an append confirm writes no account", registry.list().isEmpty())
        assertEquals(null, registry.active())
    }

    private companion object {
        const val SECRET =
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
    }
}
