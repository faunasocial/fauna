package com.fauna.app.ui.viewmodel

import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiFamilyGuardianInfo
import com.fauna.ffi.FfiFamilyStatus
import com.fauna.ffi.FfiKidsAppEligibility
import com.fauna.ffi.FfiSupervisionSnapshot
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.`when` as whenever

/**
 * [KidsEligibilityVM] — the kids build's three-way answer
 * (`account-age-band.md` § The account age band → *Kids-app eligibility while
 * it is unknown*, clauses (b)–(e)). The rule itself is shared Rust
 * (`kids_app_eligibility`, unit-tested in `fauna-client-family`); this pins
 * what the VM feeds it and when. FFI-free: the decision is a stand-in that
 * mirrors the shared rule over mocks (a read naming a guardian or a snapshot
 * → eligible, a read naming none → ineligible, nothing → unknown), and every
 * collaborator is mocked as in [SupervisedIndicatorVMTest].
 */
@ExperimentalCoroutinesApi
class KidsEligibilityVMTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val snapshot = FfiSupervisionSnapshot(
        supervisedBy = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 4 }, handle = "parent@example.org"),
        contentPolicy = null,
        contentNotify = false,
        screenTime = null,
    )

    /** Mirrors the shared rule; the real one is tested in Rust. */
    private val decide: (FfiFamilyStatus?, FfiSupervisionSnapshot?) -> FfiKidsAppEligibility = { read, snap ->
        when {
            read != null && read.supervisedBy != null -> FfiKidsAppEligibility.ELIGIBLE
            read != null -> FfiKidsAppEligibility.INELIGIBLE
            snap != null -> FfiKidsAppEligibility.ELIGIBLE
            else -> FfiKidsAppEligibility.UNKNOWN
        }
    }

    private class State(var actor: String?, var snapshot: FfiSupervisionSnapshot?)

    /** One scripted `familyStatus()` answer per call; a pending one never completes. */
    private class Reads {
        val answers = ArrayDeque<CompletableDeferred<FfiFamilyStatus>>()
        fun pending() = CompletableDeferred<FfiFamilyStatus>().also { answers.addLast(it) }
        fun failing() = pending().also { it.completeExceptionally(RuntimeException("unreachable")) }
        fun ok(status: FfiFamilyStatus) = pending().also { it.complete(status) }
    }

    private data class Fixture(
        val vm: KidsEligibilityVM,
        val state: State,
        val accountStores: AccountStores,
        val reads: Reads,
        val tick: MutableSharedFlow<Unit>,
    )

    private fun makeVm(actor: String?, snap: FfiSupervisionSnapshot?, firstRead: (Reads) -> Unit): Fixture {
        val api = mock(ApiClient::class.java)
        val accountStores = mock(AccountStores::class.java)
        val state = State(actor, snap)
        val reads = Reads()
        firstRead(reads)
        val tick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
        whenever(api.reconnectTick).thenReturn(tick)
        whenever(accountStores.activeActorHex()).thenAnswer { state.actor }
        whenever(accountStores.supervisionSnapshot()).thenAnswer { state.snapshot }
        val vm = KidsEligibilityVM(api, accountStores, decide) {
            (reads.answers.removeFirstOrNull() ?: CompletableDeferred()).await()
        }
        return Fixture(vm, state, accountStores, reads, tick)
    }

    private fun status(supervised: Boolean): FfiFamilyStatus {
        val s = mock(FfiFamilyStatus::class.java)
        whenever(s.supervisedBy).thenReturn(if (supervised) snapshot.supervisedBy else null)
        return s
    }

    private fun closerOf(accountStores: AccountStores): () -> Unit {
        val registerCall = mockingDetails(accountStores).invocations
            .single { it.method.name == "registerCloser" }
        @Suppress("UNCHECKED_CAST")
        return registerCall.arguments[1] as () -> Unit
    }

    @Test
    fun aSupervisedSnapshotSeedsEligibleBeforeAnyRead() {
        val f = makeVm(ACTOR_A, snapshot) { it.failing() }
        assertEquals(FfiKidsAppEligibility.ELIGIBLE, f.vm.eligibility.value)
        assertFalse("a decided answer paints no error", f.vm.readFailedWhileUnknown.value)
    }

    @Test
    fun noSnapshotAndAPendingReadIsUnknown() {
        val f = makeVm(ACTOR_A, null) { it.pending() }
        assertEquals(FfiKidsAppEligibility.UNKNOWN, f.vm.eligibility.value)
        assertFalse(f.vm.readFailedWhileUnknown.value)
    }

    @Test
    fun aFailedReadWhileUnknownStaysUnknownAndRaisesTheError() {
        val f = makeVm(ACTOR_A, null) { it.failing() }
        assertEquals(FfiKidsAppEligibility.UNKNOWN, f.vm.eligibility.value)
        assertTrue(f.vm.readFailedWhileUnknown.value)

        // The read that succeeds decides and clears the error.
        f.reads.ok(status(supervised = false))
        f.tick.tryEmit(Unit)
        assertEquals(FfiKidsAppEligibility.INELIGIBLE, f.vm.eligibility.value)
        assertFalse(f.vm.readFailedWhileUnknown.value)
    }

    @Test
    fun aFailedReadAfterEligibleStaysEligible() {
        val f = makeVm(ACTOR_A, null) { it.ok(status(supervised = true)) }
        assertEquals(FfiKidsAppEligibility.ELIGIBLE, f.vm.eligibility.value)

        f.reads.failing()
        f.tick.tryEmit(Unit)
        assertEquals(FfiKidsAppEligibility.ELIGIBLE, f.vm.eligibility.value)
        assertFalse(f.vm.readFailedWhileUnknown.value)
    }

    @Test
    fun anAccountSwitchReseedsFromTheIncomingAccount() {
        val f = makeVm(ACTOR_A, snapshot) { it.failing() }
        assertEquals(FfiKidsAppEligibility.ELIGIBLE, f.vm.eligibility.value)

        // The incoming account was never seen supervised here and its first
        // read has not answered: unknown, never the outgoing ward's shell.
        f.state.actor = ACTOR_B
        f.state.snapshot = null
        f.reads.pending()
        closerOf(f.accountStores)()
        assertEquals(FfiKidsAppEligibility.UNKNOWN, f.vm.eligibility.value)
        assertFalse(f.vm.readFailedWhileUnknown.value)
    }

    private companion object {
        val ACTOR_A = "aa11".repeat(16)
        val ACTOR_B = "bb22".repeat(16)
    }
}
