package com.fauna.app.ui.viewmodel

import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ScreenTimeStore
import com.fauna.ffi.FfiFamilyGuardianInfo
import com.fauna.ffi.FfiFamilyStatus
import com.fauna.ffi.FfiReachPolicy
import com.fauna.ffi.FfiScreenTimePolicy
import com.fauna.ffi.FfiSupervisionSnapshot
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * [SupervisedIndicatorVM]'s re-seed on an in-process account switch
 * (family-safety.md § Content policy, clause 2's `supervised-indicator`
 * twin, closed alongside [com.fauna.app.core.ContentPolicyStore] and
 * [ScreenTimeStore] by ): the switch closer runs
 * AFTER `AccountStores`'s active pointer already names the INCOMING actor,
 * so without a re-seed here the global chrome keeps naming no guardian for
 * the incoming supervised ward until a read succeeds.
 *
 * FFI-free (mocks every collaborator, mirrors `AccountSettingsVMTest`) —
 * `init` collects [ApiClient.reconnectTick] on `viewModelScope`, so
 * `Dispatchers.setMain(UnconfinedTestDispatcher())` is required exactly as
 * there.
 */
@ExperimentalCoroutinesApi
class SupervisedIndicatorVMTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun snapshotFor(guardianHandle: String) = FfiSupervisionSnapshot(
        supervisedBy = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 4 }, handle = guardianHandle),
        contentPolicy = null,
        contentNotify = false,
        screenTime = null,
    )

    private class AccountState(var activeActor: String?, var snapshot: FfiSupervisionSnapshot?)

    private fun makeVm(initialActor: String?, initialSnapshot: FfiSupervisionSnapshot?):
        Triple<SupervisedIndicatorVM, AccountState, AccountStores> {
        val api = mock(ApiClient::class.java)
        val screenTimeStore = mock(ScreenTimeStore::class.java)
        val accountStores = mock(AccountStores::class.java)
        val state = AccountState(initialActor, initialSnapshot)
        // Stubbed BEFORE construction: `init` collects this the moment the VM
        // is built (mirrors AccountSettingsVMTest).
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow(replay = 0, extraBufferCapacity = 1))
        whenever(accountStores.activeActorHex()).thenAnswer { state.activeActor }
        whenever(accountStores.supervisionSnapshot()).thenAnswer { state.snapshot }
        val vm = SupervisedIndicatorVM(api, screenTimeStore, accountStores)
        return Triple(vm, state, accountStores)
    }

    private fun closerOf(accountStores: AccountStores): () -> Unit {
        val registerCall = mockingDetails(accountStores).invocations
            .single { it.method.name == "registerCloser" }
        @Suppress("UNCHECKED_CAST")
        return registerCall.arguments[1] as () -> Unit
    }

    @Test
    fun `an offline switch re-seeds the incoming actor's guardian handle`() {
        val (vm, state, accountStores) = makeVm(initialActor = "actor-a", initialSnapshot = null)
        assertNull("actor A has no persisted snapshot", vm.supervisedByHandle.value)

        val closer = closerOf(accountStores)
        state.activeActor = "actor-b"
        state.snapshot = snapshotFor("guardian-b")
        closer.invoke()

        assertEquals(
            "an offline in-process switch must re-seed the INCOMING actor's " +
                "guardian, not leave the indicator blank until a read succeeds " +
                "(family-safety.md § Content policy, clause 2)",
            "guardian-b",
            vm.supervisedByHandle.value,
        )
    }

    private val window = FfiScreenTimePolicy(windowStart = 1260u, windowEnd = 420u, dailyMinutes = null)

    /** A successful reply whose policy sets a bedtime window; whether the
     *  gated fold carries it is decided by [supervisedBy], as shared Rust's is. */
    private fun statusWithWindow(supervisedBy: FfiFamilyGuardianInfo?) = FfiFamilyStatus(
        supervisedBy = supervisedBy,
        policy = FfiReachPolicy(
            contactApproval = false,
            unknownSenderMail = "allow",
            federationContact = true,
            feedSources = "allow",
            contentPolicy = null,
            screenTime = window,
            contentNotify = false,
            unknownPeerDm = null,
        ),
        wards = emptyList(),
        incomingTransfers = emptyList(),
        usageTodayMinutes = null,
        contactRequests = emptyList(),
        feedRequests = emptyList(),
        ageBand = null,
        supervision = supervisedBy?.let {
            FfiSupervisionSnapshot(supervisedBy = it, contentPolicy = null, contentNotify = false, screenTime = window)
        },
    )

    /** Construct the VM over a status read that succeeds with [reply]; `init`'s
     *  refresh runs eagerly on the unconfined Main dispatcher. */
    private fun screenTimeFedBy(reply: FfiFamilyStatus): ScreenTimeStore {
        val api = mock(ApiClient::class.java)
        val screenTimeStore = mock(ScreenTimeStore::class.java)
        val accountStores = mock(AccountStores::class.java)
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow(replay = 0, extraBufferCapacity = 1))
        runBlocking { whenever(api.familyStatus()).thenReturn(reply) }
        SupervisedIndicatorVM(api, screenTimeStore, accountStores)
        return screenTimeStore
    }

    @Test
    fun `a supervised read feeds the lock its window from the fold`() {
        val screenTimeStore = screenTimeFedBy(statusWithWindow(snapshotFor("guardian").supervisedBy))
        verify(screenTimeStore).setWardScreenTime(window, "guardian", null)
    }

    @Test
    fun `a read whose policy names no guardian feeds the lock nothing`() {
        // The raw document still names a bedtime window; only the gated fold
        // keeps it off an unsupervised viewer
        // (family-client-enforcement.md § Implementation status today).
        val screenTimeStore = screenTimeFedBy(statusWithWindow(null))
        verify(screenTimeStore).setWardScreenTime(null, null, null)
    }

    @Test
    fun `a same-actor drop does not resurrect the outgoing actor's own guardian`() {
        val (vm, _, accountStores) = makeVm(
            initialActor = "actor-a",
            initialSnapshot = snapshotFor("guardian-a"),
        )
        assertEquals("guardian-a", vm.supervisedByHandle.value)

        // A debug `TestAgent` `reset`/`logout` arm whose credential wipe does
        // not reach the registry: the active pointer and its snapshot are
        // UNCHANGED here, exactly like that failure mode.
        val closer = closerOf(accountStores)
        closer.invoke()

        assertNull(
            "a same-actor drop must land blank, never resurrect the SAME " +
                "actor's own last-known guardian",
            vm.supervisedByHandle.value,
        )
    }
}
