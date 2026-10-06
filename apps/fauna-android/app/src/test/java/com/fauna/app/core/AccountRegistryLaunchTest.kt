package com.fauna.app.core

import com.fauna.ffi.FfiAccountRegistry
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.fauna_launch_machine.LaunchMachine
import uniffi.fauna_launch_machine.LaunchObserver
import uniffi.fauna_launch_machine.LaunchPhase
import uniffi.fauna_launch_machine.LaunchWizardEntry
import uniffi.fauna_launch_machine.PendingFactoryResetRecord

/**
 * The CR-1 resume, exercised end-to-end through the REAL shared
 * `FfiAccountRegistry` + `LaunchMachine` across the `FfiSecretStore` seam
 * (`nest/common.md` Section Client-state recoverability): a factory reset whose
 * claim code was minted into the active account's per-actor slot before the
 * dispatch routes the next launch to the pre-filled claim, ahead of every other
 * row — through the same `launchPersistence()` `AppLaunchVM` hydrates the
 * record from.
 *
 * FFI-touching -> runs green via `just android-host-test`, which puts the
 * locally built `libfauna_ffi.so` on the JVM's `jna.library.path` (the
 * desktop-JNA unit-test path). This replaced the pre-registry upgrade-boot pin
 * (the retired single-slot bridge, `long-term-store.md` § Downgrade mirror +
 * abandoned-append recovery).
 */
class AccountRegistryLaunchTest {

    @Test
    fun aPendingFactoryResetRoutesToThePrefilledClaim() = runBlocking {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
        registry.addAccount(VALID_SECRET_HEX, "https://nest.example", null)
        val persistence = registry.launchPersistence()
        persistence.savePendingFactoryReset(
            PendingFactoryResetRecord(
                nestUrl = "https://nest.example",
                handle = "alice@example.com",
                claimCode = "CLAIM-ABC123",
                mintedAtSecs = 0uL,
            ),
        )

        val machine = LaunchMachine(NoopObserver, persistence)
        machine.start()

        assertEquals(
            LaunchPhase.WizardAt(LaunchWizardEntry.PENDING_FACTORY_RESET),
            machine.snapshot().phase,
        )
        assertEquals("CLAIM-ABC123", persistence.loadPendingFactoryReset()?.claimCode)
    }

    private object NoopObserver : LaunchObserver {
        override fun onChanged() {}
    }

    private companion object {
        const val VALID_SECRET_HEX =
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
    }
}
