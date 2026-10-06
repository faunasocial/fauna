package com.fauna.app.core

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiFamilyGuardianInfo
import com.fauna.ffi.FfiScreenTimePolicy
import com.fauna.ffi.FfiSupervisionSnapshot
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * [ScreenTimeStore]'s re-seed on an in-process account switch
 * (family-safety.md § Content policy, clause 2's screen-time twin, closed
 * alongside [ContentPolicyStore] by ): the switch
 * closer runs AFTER `AccountStores`'s active pointer already names the
 * INCOMING actor (`AccountSettingsVM.switchAccount`'s `registry.setActive`
 * precedes `actorScope.dropActorScopedState()`), so without a re-seed here an
 * offline switch leaves the incoming ward's bedtime window unenforced — an
 * airplane-mode bypass of exactly the window this store exists to hold.
 *
 * FFI-touching ([FfiUsageHeartbeat] is a real UniFFI object [ScreenTimeStore]
 * constructs directly, with no injection seam to mock it out) → runs green
 * only via `just android-host-test` (host JNA + a host-target
 * `libfauna_ffi.so`), same as [ContentPolicyStoreTest]'s FFI-touching arms.
 * `AccountStores` and `ApiClient` stay plain Mockito mocks — only the
 * `@ApplicationContext` (Robolectric) and the heartbeat (host JNA) need to be
 * real.
 *
 * The WINDOW half is used for these assertions rather than the budget half,
 * deliberately: it is pure local clock
 * (`fauna_core::screen_time::ScreenTimePolicy::in_window`), and a policy with
 * `windowStart == windowEnd` reads fail-closed (locked) UNCONDITIONALLY —
 * deterministic regardless of the real wall-clock time this test happens to
 * run at (testing.md convention 14: no wall-clock timing dependence). The
 * budget half cannot give the same guarantee here: a restored snapshot never
 * carries a usage total (clause 2 — "the day's usage total is deliberately
 * NOT persisted"), and `lock_state` does not evaluate the budget at all until
 * a total is known.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ScreenTimeStoreTest {

    /** Locked all day regardless of the real clock — see the class doc. */
    private fun alwaysLockedWindow() =
        FfiScreenTimePolicy(windowStart = 0u, windowEnd = 0u, dailyMinutes = null)

    private fun snapshotFor(guardianHandle: String) = FfiSupervisionSnapshot(
        supervisedBy = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 7 }, handle = guardianHandle),
        contentPolicy = null,
        contentNotify = false,
        screenTime = alwaysLockedWindow(),
    )

    private class AccountState(var activeActor: String?, var snapshot: FfiSupervisionSnapshot?)

    private fun makeStore(initialActor: String?, initialSnapshot: FfiSupervisionSnapshot?):
        Triple<ScreenTimeStore, AccountState, AccountStores> {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        val api = mock(ApiClient::class.java)
        val accountStores = mock(AccountStores::class.java)
        val state = AccountState(initialActor, initialSnapshot)
        whenever(accountStores.activeActorHex()).thenAnswer { state.activeActor }
        whenever(accountStores.supervisionSnapshot()).thenAnswer { state.snapshot }
        val store = ScreenTimeStore(context, api, accountStores)
        return Triple(store, state, accountStores)
    }

    private fun closerOf(accountStores: AccountStores): () -> Unit {
        val registerCall = mockingDetails(accountStores).invocations
            .single { it.method.name == "registerCloser" }
        @Suppress("UNCHECKED_CAST")
        return registerCall.arguments[1] as () -> Unit
    }

    @Test
    fun `an offline switch re-seeds the incoming actor's restored window`() {
        val (store, state, accountStores) = makeStore(initialActor = "actor-a", initialSnapshot = null)
        assertNull("actor A has no persisted snapshot, so no lock at construction", store.lockMessage.value)

        val closer = closerOf(accountStores)
        state.activeActor = "actor-b"
        state.snapshot = snapshotFor("guardian-b")
        closer.invoke()

        val message = store.lockMessage.value
        assertNotNull(
            "an offline in-process switch must re-seed the INCOMING actor's " +
                "restored window, not leave the ward unenforced until a read " +
                "succeeds (family-safety.md § Content policy, clause 2)",
            message,
        )
        assertTrue("the lock must name the INCOMING actor's guardian", message!!.contains("guardian-b"))
    }

    @Test
    fun `switching to an actor with no persisted snapshot leaves the lock blank`() {
        val (store, state, accountStores) = makeStore(
            initialActor = "actor-a",
            initialSnapshot = snapshotFor("guardian-a"),
        )
        assertNotNull("actor A's restored window must lock at construction", store.lockMessage.value)

        val closer = closerOf(accountStores)
        state.activeActor = "actor-b"
        state.snapshot = null
        closer.invoke()

        assertNull(
            "an unsupervised incoming actor must render nothing, not the " +
                "outgoing ward's window",
            store.lockMessage.value,
        )
    }

    @Test
    fun `a same-actor drop does not resurrect the outgoing actor's own window`() {
        val (store, _, accountStores) = makeStore(
            initialActor = "actor-a",
            initialSnapshot = snapshotFor("guardian-a"),
        )
        assertNotNull("actor A's restored window must lock at construction", store.lockMessage.value)

        // A debug `TestAgent` `reset`/`logout` arm whose credential wipe does
        // not reach the registry (`FileSecretBackend` e2e mode: `SecureStorage
        // .clear()` only empties the DIFFERENT `fauna_secure_prefs` file) —
        // the active pointer and its snapshot are UNCHANGED here, exactly
        // like that failure mode.
        val closer = closerOf(accountStores)
        closer.invoke()

        assertNull(
            "a same-actor drop must land blank, never resurrect the SAME " +
                "actor's own last-known window under what is supposed to be a " +
                "blank reset",
            store.lockMessage.value,
        )
    }
}
