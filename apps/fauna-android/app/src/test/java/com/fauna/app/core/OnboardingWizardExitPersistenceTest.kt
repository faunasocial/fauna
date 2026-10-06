package com.fauna.app.core

import com.fauna.ffi.FfiAccountRegistry
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.fauna_launch_machine.LaunchMachine
import uniffi.fauna_launch_machine.LaunchObserver
import uniffi.fauna_launch_machine.LaunchPhase
import uniffi.fauna_launch_machine.LaunchWizardEntry

/**
 * The wizard-exit slot writers `OnboardingHost` calls — `persistAwaitingDns`,
 * `persistLoggedIn`, `persistPendingInvite` — exercised end-to-end through the
 * REAL shared `FfiAccountRegistry` + `LaunchMachine` across the `FfiSecretStore`
 * seam: the android twin of apple's `KeychainSecretStoreTests.swift`
 * ("Wizard-exit slot writers"). `AppLaunchVM` reads each slot back through the
 * same registry-backed `LaunchPersistence` the machine branched on.
 *
 * FFI-touching -> runs green via `just android-host-test` (see
 * [AccountRegistryLaunchTest] for the mechanism).
 *
 * History: until 2026-07-23 android wrote these exits to its single-slot
 * native rows (`onboarding.md` § "The UniFFI apps' legacy-slot gap"), which
 * dropped a handle-less deferred-DNS exit and lost every slot on a
 * multi-account install. Those rows are retired altogether now
 * (`long-term-store.md` § Downgrade mirror + abandoned-append recovery).
 */
class OnboardingWizardExitPersistenceTest {

    /**
     * **The `test_smoke_i` regression.** The deferred-DNS exit is reached with
     * NO handle yet — the wizard goes identity → provisioning →
     * `dns_post_instructions` with no handle stage — and the resume must
     * still survive a force-quit + relaunch: an empty handle is data in the
     * per-actor slot, never a reason to drop the row.
     */
    @Test
    fun deferredDnsSlotWithNoHandleYetSurvivesTheRelaunch() = runBlocking {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))

        registry.persistAwaitingDns(
            secretHex = SECRET_A,
            nestUrl = "https://nest.example.com",
            handle = "", // the exit has none yet — this is the whole point
            dnsRecordsJson = """[{"record_type":"A","name":"@","value":"203.0.113.7"}]""",
            claimCode = "DNS-CODE",
        )

        val rec = registry.launchPersistence().loadAwaitingDns()
        assertEquals("", rec?.handle)
        assertEquals("DNS-CODE", rec?.claimCode)
        assertEquals(true, rec?.dnsRecordsJson?.contains("203.0.113.7"))

        // The mechanism that actually matters in production: a fresh LaunchMachine
        // over the SAME store routes the relaunch straight to the "Almost ready"
        // wizard entry, off this exact record -- not falling through to
        // HANDLE_ENTRY.
        val machine = LaunchMachine(NoopObserver, registry.launchPersistence())
        machine.start()
        assertEquals(
            LaunchPhase.WizardAt(LaunchWizardEntry.AWAITING_MANUAL_DNS),
            machine.snapshot().phase,
        )
    }

    /**
     * A second account on the install never hides the deferred-DNS slot: it is
     * written per-actor for the identity the wizard acts as.
     */
    @Test
    fun deferredDnsSlotSurvivesOnAMultiAccountInstall() {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
        registry.addAccount(SECRET_B, "https://b.example", null)

        registry.persistAwaitingDns(
            secretHex = SECRET_A,
            nestUrl = "https://nest.example.com",
            handle = "alice@example.com",
            dnsRecordsJson = "[]",
            claimCode = "DNS-CODE",
        )

        assertEquals("DNS-CODE", registry.launchPersistence().loadAwaitingDns()?.claimCode)

        // …and the claim terminal clears it.
        registry.clearAwaitingDns()
        assertNull(registry.launchPersistence().loadAwaitingDns())
    }

    /**
     * **Row 51 — the `LoggedIn` terminal.** `persist_logged_in` records the
     * identity's home nest per-actor, so the next launch routes on it rather
     * than silently returning a completed onboarding to the handle-entry page
     * (`onboarding.md` § App-launch routing) — the shared call apple and
     * windows make too.
     */
    @Test
    fun loggedInSlotSurvivesOnAMultiAccountInstall() {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
        registry.addAccount(SECRET_B, "https://b.example", null)

        registry.persistLoggedIn(
            secretHex = SECRET_A,
            nestUrl = "https://nest.example.com",
            deviceId = "dev-a",
            reachIpv4 = null,
        )

        assertEquals(
            "https://nest.example.com",
            registry.launchPersistence().loadNestUrl(),
        )
    }

    /** The pending-invite twin, for the same multi-account reason. */
    @Test
    fun pendingInviteSlotSurvivesOnAMultiAccountInstall() {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
        registry.addAccount(SECRET_B, null, null)

        registry.persistPendingInvite(
            secretHex = SECRET_A,
            nestUrl = "https://nest.example.com",
            handle = "alice@example.com",
            requestId = "req-1",
            statusJson = "{}",
        )

        assertEquals("req-1", registry.launchPersistence().loadPendingInvite()?.requestId)
    }

    private object NoopObserver : LaunchObserver {
        override fun onChanged() {}
    }

    private companion object {
        // Two distinct valid 32-byte identity secrets (hex).
        const val SECRET_A =
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
        const val SECRET_B =
            "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f"
    }
}
