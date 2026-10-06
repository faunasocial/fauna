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
 * `IdentityCreatedVM.confirm` runs the shared confirm-identity moment
 * (`confirmIdentity(secret, append)`) in both modes, over a REAL
 * `FfiAccountRegistry` (the host-built `libfauna_ffi`, an in-memory store).
 *
 * The append case is the regression this
 * pins: an "Add account" confirm must register NOTHING — no account, no moved
 * active pointer — so an append run abandoned after its confirm leaves the live
 * session exactly as it was (`long-term-store.md` § Downgrade mirror +
 * abandoned-append recovery). Until 2026-09-28 android kept a raw single-slot
 * secret write here instead; the appended identity now stays in the wizard
 * machine until the append terminal registers it.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class IdentityCreatedVMTest {

    private val backend = MemoryBackend()
    private val registry = FfiAccountRegistry(LogicalSecretStore(backend))

    private fun makeVm(appendMode: Boolean): IdentityCreatedVM {
        val host = mock(OnboardingHost::class.java)
        val machine = mock(OnboardingMachine::class.java)
        whenever(host.machine).thenReturn(machine)
        whenever(host.appendMode).thenReturn(appendMode)
        whenever(machine.confirmGeneratedIdentity()).thenReturn(SECRET)
        return IdentityCreatedVM(host, registry)
    }

    @Test
    fun normalOnboard_registersAndActivatesTheIdentity() {
        makeVm(appendMode = false).confirm()

        val active = registry.active()
        assertNotNull("the first-run confirm activates the new identity", active)
        assertEquals(SECRET, registry.sessionMaterial(active!!)?.secretHex)
    }

    /** An abandoned append registers nothing: no account, nothing active. */
    @Test
    fun appendMode_registersNothing() {
        makeVm(appendMode = true).confirm()

        assertTrue("an append confirm writes no account", registry.list().isEmpty())
        assertEquals(null, registry.active())
    }

    /** With a live account, the append confirm leaves it the only, active one. */
    @Test
    fun appendMode_leavesTheLiveAccountActive() {
        val live = registry.confirmIdentity(LIVE_SECRET, false)

        makeVm(appendMode = true).confirm()

        assertEquals(live, registry.active())
        assertEquals(listOf(live), registry.list().map { it.actorId })
    }

    private companion object {
        const val SECRET =
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
        const val LIVE_SECRET =
            "2120201f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403"
    }
}
