package com.fauna.app.core

import android.content.Context
import com.fauna.ffi.FfiAccountRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import uniffi.fauna_launch_machine.LaunchPersistence

/**
 * The append-mode ("Add account") pending-invite adoption on android:
 * [OnboardingHost.persistPendingInviteSlot] registers the joiner, writes its
 * per-actor pending-invite slot, activates it and — in append mode only — drops the
 * OUTGOING account's live actor-scoped state (`onboarding.md` § Multi-account: "the
 * append glue adopts on the submit return"). The android twin of
 * `test_apple_add_account_pending_invite_submit_switches_the_live_session`; this leg
 * shipped its append adoption without a pin of its own.
 *
 * Real shared `FfiAccountRegistry` over an in-memory `FfiSecretStore`; the wizard
 * machine is seeded onto a pending invite through its own `seedIdentity` /
 * `seedPendingInvite` (the same hydration the resume path uses), so no nest is
 * needed. FFI-touching -> runs green via `just android-host-test`.
 */
class OnboardingHostPendingInviteAdoptionTest {

    private class Fixture(
        val host: OnboardingHost,
        val registry: FfiAccountRegistry,
        val actorScope: ActorScope,
        val outgoingActor: String,
    )

    private fun makeHost(): Fixture {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
        // The outgoing account — the registry's active one before the adoption.
        registry.addAccount(SECRET_OUTGOING, "https://outgoing.example", null)
        val outgoingActor = registry.active()
        assertNotNull("the seeded outgoing account must be active", outgoingActor)

        val actorScope = mock(ActorScope::class.java)
        val host = OnboardingHost(
            sessionAccount = mock(SessionAccount::class.java),
            registry = registry,
            launchPersistence = mock(LaunchPersistence::class.java),
            actorScope = actorScope,
            accountStores = mock(AccountStores::class.java),
            context = mock(Context::class.java),
        )
        // The joiner's wizard, sitting on a pending invite request.
        host.machine.seedIdentity(SECRET_JOINER)
        host.machine.seedPendingInvite(
            "https://nest.example.com", "joiner@example.com", "req-1",
            """{"PendingReview":{"request_id":"req-1","last_checked_ms":0}}""",
        )
        return Fixture(host, registry, actorScope, outgoingActor!!)
    }

    @Test
    fun appendModeSubmitRegistersActivatesTheJoinerAndDropsTheOutgoingSession() {
        val f = makeHost()
        f.host.appendMode = true

        f.host.persistPendingInviteSlot()

        val actors = f.registry.list().map { it.actorId }
        assertEquals("the joiner is registered beside the original", 2, actors.size)
        assertTrue("the original account survives (no-user-data-loss)", f.outgoingActor in actors)
        val joiner = f.registry.active()
        assertNotEquals("the submit activates the joiner", f.outgoingActor, joiner)
        assertEquals(
            "the joiner's per-actor pending-invite slot is written",
            "req-1",
            f.registry.launchPersistence().loadPendingInvite()?.requestId,
        )
        // The live session must follow the registry: the outgoing account's
        // actor-scoped state is dropped, the same drop `AccountSettingsVM
        // .switchAccount` runs.
        verify(f.actorScope).dropActorScopedState()
    }

    @Test
    fun nonAppendSubmitWritesTheSlotAndDropsNothing() {
        val f = makeHost()
        f.host.appendMode = false

        f.host.persistPendingInviteSlot()

        assertEquals(
            "req-1",
            f.registry.launchPersistence().loadPendingInvite()?.requestId,
        )
        verify(f.actorScope, never()).dropActorScopedState()
    }

    private companion object {
        // Two distinct valid 32-byte identity secrets (hex), built rather than spelled.
        val SECRET_OUTGOING = "01".repeat(32)
        val SECRET_JOINER = "02".repeat(32)
    }
}
