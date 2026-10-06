package com.fauna.app.core

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiContentPolicy
import com.fauna.ffi.FfiFamilyGuardianInfo
import com.fauna.ffi.FfiFamilyStatus
import com.fauna.ffi.FfiReachPolicy
import com.fauna.ffi.FfiSpamPreferences
import com.fauna.ffi.FfiSupervisionSnapshot
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * [ContentPolicyStore]'s refresh semantics under the unfetched-policy ruling
 * (family-safety.md § Content policy, ratified 2026-08-02): **a failed read
 * keeps the last-known state in force** — "read failed" and "read says
 * unsupervised" are different facts, and only the second (or an identity
 * change) may clear a loaded guardian floor. This is the android arm of the
 * probe shape: load a floor, fail the next read, assert the
 * verdict inputs still carry the floor.
 *
 * Latency-independence (testing.md convention 14): the negative assert ("the
 * failed refresh did not clear") anchors on a causal barrier — the same
 * refresh's *other* half returns changed thresholds, so observing them proves
 * the refresh ran to completion before the floor is inspected. Budgets are
 * generous ceilings, not expected latencies.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ContentPolicyStoreTest {

    private companion object {
        const val BUDGET_MS = 10_000L
    }

    private fun floors(nsfw: String = "block") =
        FfiContentPolicy(nsfw = nsfw, spam = "inherit", phishing = "inherit", commercial = "inherit")

    private val guardian = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 2 }, handle = "guardian")

    private fun supervisedStatus(floor: FfiContentPolicy?) = FfiFamilyStatus(
        supervisedBy = guardian,
        policy = FfiReachPolicy(
            contactApproval = false,
            unknownSenderMail = "allow",
            federationContact = true,
            feedSources = "allow",
            contentPolicy = floor,
            screenTime = null,
            contentNotify = true,
            unknownPeerDm = null,
        ),
        wards = emptyList(),
        incomingTransfers = emptyList(),
        usageTodayMinutes = null,
        contactRequests = emptyList(),
        feedRequests = emptyList(),
        // family-safety.md § The account age band — this file's subject is the
        // content-policy floor, which no band participates in.
        ageBand = null,
        // The gated fold shared Rust attaches to every real reply
        // (`FfiFamilyStatus.supervision`) — the half this store reads.
        supervision = FfiSupervisionSnapshot(
            supervisedBy = guardian,
            contentPolicy = floor,
            contentNotify = true,
            screenTime = null,
        ),
    )

    private fun prefs(spam: Int, phishing: Int) = FfiSpamPreferences(
        spamThreshold = spam.toUShort(),
        phishingThreshold = phishing.toUShort(),
    )

    /**
     * The two read outcomes `ContentPolicyStore.refresh()` polls, as mutable
     * fields a test can flip AFTER construction without re-stubbing the mock.
     * `ContentPolicyStore.init` starts `scope.launch { api.reconnectTick
     * .collect { refresh() } }` on a raw `Dispatchers.IO` scope the instant the
     * store is built — a real background thread this test does not control
     * (unlike a ViewModel's `viewModelScope`, which a test dispatcher pins).
     * Calling `whenever(api.familyStatus())` again from the test body races
     * that thread's call to `api.reconnectTick`'s getter for Mockito's
     * "last invocation" tracking: both are invocations on the SAME mock, and
     * whichever lands last between the `when(...)` call and its `.thenReturn`/
     * `.thenThrow` steals the stub, surfacing as `WrongTypeOfReturnValue:
     * ... cannot be returned by getReconnectTick()`. Installing one stable
     * `thenAnswer` per method before construction and mutating these fields
     * instead means the test thread never calls `whenever` on this mock again.
     */
    private class ApiState(
        var status: Result<FfiFamilyStatus>,
        var prefs: Result<FfiSpamPreferences>,
    )

    private fun makeLoaded(): Triple<ContentPolicyStore, ApiState, AccountStores> = runBlocking {
        val api = mock(ApiClient::class.java)
        val accountStores = mock(AccountStores::class.java)
        val state = ApiState(
            status = Result.success(supervisedStatus(floors())),
            prefs = Result.success(prefs(100, 100)),
        )
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.familyStatus()).thenAnswer { state.status.getOrThrow() }
        whenever(api.getSpamPreferences()).thenAnswer { state.prefs.getOrThrow() }
        val store = ContentPolicyStore(api, accountStores)
        withTimeout(BUDGET_MS) { store.inputs.first { it.contentPolicy != null } }
        Triple(store, state, accountStores)
    }

    @Test
    fun `construction restores the persisted floor before any read succeeds`() = runBlocking {
        val api = mock(ApiClient::class.java)
        val accountStores = mock(AccountStores::class.java)
        // The status read FAILS for the whole test — the persisted snapshot is
        // the only possible source of the floor asserted below (clause 2:
        // "loaded at launch ahead of the first read"; the cold-offline-launch
        // case the ruling exists for).
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.familyStatus()).thenAnswer {
            throw RuntimeException("nest unreachable at cold launch")
        }
        whenever(api.getSpamPreferences()).thenAnswer { prefs(700, 900) }
        whenever(accountStores.supervisionSnapshot()).thenReturn(
            com.fauna.ffi.FfiSupervisionSnapshot(
                supervisedBy = FfiFamilyGuardianInfo(actorId = ByteArray(3) { 1 }, handle = "guardian"),
                contentPolicy = floors(),
                contentNotify = true,
                screenTime = null,
            ),
        )

        val store = ContentPolicyStore(api, accountStores)
        // Causal barrier: the prefs half landing proves construction's refresh
        // ran to completion — so the floor observed below was restored AND
        // kept across the failed read, not merely not-yet-cleared.
        val after = withTimeout(BUDGET_MS) {
            store.inputs.first { it.ownSpamPermille == 700.toUShort() }
        }

        assertNotNull(
            "the restored floor must bind before/without a successful read " +
                "(family-safety.md § Content policy, unfetched-policy clause 2)",
            after.contentPolicy,
        )
        assertEquals("block", after.contentPolicy!!.nsfw)
        assertEquals(
            "content_notify restores too — the field tui's mutation run proved uncovered",
            true,
            after.contentNotify,
        )
    }

    @Test
    fun `a failed status read keeps the loaded guardian floor`() = runBlocking {
        val (store, state, _) = makeLoaded()

        // The status read now fails while the prefs read returns NEW values —
        // observing the changed thresholds is the causal barrier proving the
        // failed refresh completed before the floor is inspected.
        state.status = Result.failure(RuntimeException("nest unreachable"))
        state.prefs = Result.success(prefs(700, 900))
        store.refresh()
        val after = withTimeout(BUDGET_MS) {
            store.inputs.first { it.ownSpamPermille == 700.toUShort() }
        }

        assertNotNull(
            "a failed status read must keep the loaded floor " +
                "(family-safety.md § Content policy, unfetched-policy clause 1)",
            after.contentPolicy,
        )
        assertEquals("block", after.contentPolicy!!.nsfw)
        assertEquals("the Notify knob keeps its last-known value too", true, after.contentNotify)
    }

    @Test
    fun `a successful unsupervised read clears the floor`() = runBlocking {
        val (store, state, _) = makeLoaded()

        // Same status shape a graduated/unsupervised viewer gets: success, no
        // policy — the ONE read outcome that legitimately clears.
        state.status = Result.success(
            FfiFamilyStatus(
                supervisedBy = null,
                policy = null,
                wards = emptyList(),
                incomingTransfers = emptyList(),
                usageTodayMinutes = null,
                contactRequests = emptyList(),
                feedRequests = emptyList(),
                ageBand = null,
                supervision = null,
            ),
        )
        state.prefs = Result.success(prefs(700, 900))
        store.refresh()
        val after = withTimeout(BUDGET_MS) {
            store.inputs.first { it.ownSpamPermille == 700.toUShort() }
        }

        assertNull("a successful read reporting unsupervised clears the floor", after.contentPolicy)
        assertEquals(false, after.contentNotify)
    }

    @Test
    fun `a successful read whose policy names no guardian binds no floor`() = runBlocking {
        val (store, state, _) = makeLoaded()

        // A reply that still carries a policy document but names NO guardian.
        // No current nest sends one, but a client must stay correct against
        // nests of other versions, and shared Rust's gated fold hands this
        // reply no supervision at all. Reading the raw `policy` would bind its
        // floor to an unsupervised viewer — the shape linux and web were fixed
        // away from (family-client-enforcement.md § Implementation status today).
        state.status = Result.success(supervisedStatus(floors()).copy(supervisedBy = null, supervision = null))
        state.prefs = Result.success(prefs(700, 900))
        store.refresh()
        val after = withTimeout(BUDGET_MS) {
            store.inputs.first { it.ownSpamPermille == 700.toUShort() }
        }

        assertNull("a policy naming no guardian binds no floor", after.contentPolicy)
        assertEquals("nor Guardian Notify counting", false, after.contentNotify)
    }

    @Test
    fun `a page read's fold moves the guardian half`() = runBlocking {
        val (store, _, _) = makeLoaded()

        // FamilyVM's own successful read hands its fold here (web's Family
        // page feeds its floor the same way): a fold naming nothing
        // enforceable clears the half, a new one replaces it.
        store.applySupervision(null)
        assertNull(store.inputs.value.contentPolicy)
        assertEquals(false, store.inputs.value.contentNotify)

        store.applySupervision(snapshotFor("collapse"))
        assertEquals("collapse", store.inputs.value.contentPolicy?.nsfw)
        assertEquals(true, store.inputs.value.contentNotify)
        assertEquals(
            "the page read leaves the viewer's own thresholds alone",
            100.toUShort(),
            store.inputs.value.ownSpamPermille,
        )
    }

    @Test
    fun `the registered account closer clears both halves`() = runBlocking {
        val (store, _, accountStores) = makeLoaded()

        // Pull the real closer registered at construction straight from
        // Mockito's recorded invocation (the FamilyNotifyStoreTest idiom:
        // an ArgumentCaptor NPEs on the non-null `() -> Unit` param).
        val registerCall = mockingDetails(accountStores).invocations
            .single { it.method.name == "registerCloser" }
        assertEquals("content-policy", registerCall.arguments[0])
        @Suppress("UNCHECKED_CAST")
        val closer = registerCall.arguments[1] as () -> Unit

        closer.invoke()

        // Keep-on-failure makes this reset load-bearing: without it a failed
        // re-read after an account switch would render the next account
        // against the previous ward's floor (account-scoping.md § The scoping
        // taxonomy, the switch/sign-out isolation contract).
        assertEquals(ContentPolicyInputs(), store.inputs.value)
    }

    /**
     * Mutable actor-pointer state the closer tests below flip AFTER
     * construction, mirroring [ApiState]'s reason: re-stubbing `accountStores`
     * from the test thread would race the SAME mock's `registerCloser`
     * invocation Mockito records at construction.
     */
    private class AccountState(var activeActor: String?, var snapshot: com.fauna.ffi.FfiSupervisionSnapshot?)

    private fun snapshotFor(nsfw: String) = com.fauna.ffi.FfiSupervisionSnapshot(
        supervisedBy = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 5 }, handle = "guardian"),
        contentPolicy = floors(nsfw),
        contentNotify = true,
        screenTime = null,
    )

    private fun makeSwitchable(initialActor: String?, initialSnapshot: com.fauna.ffi.FfiSupervisionSnapshot?):
        Triple<ContentPolicyStore, AccountState, AccountStores> = runBlocking {
        val api = mock(ApiClient::class.java)
        val accountStores = mock(AccountStores::class.java)
        val state = AccountState(initialActor, initialSnapshot)
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.familyStatus()).thenAnswer { throw RuntimeException("offline") }
        whenever(api.getSpamPreferences()).thenAnswer { throw RuntimeException("offline") }
        whenever(accountStores.activeActorHex()).thenAnswer { state.activeActor }
        whenever(accountStores.supervisionSnapshot()).thenAnswer { state.snapshot }
        val store = ContentPolicyStore(api, accountStores)
        Triple(store, state, accountStores)
    }

    private fun closerOf(accountStores: AccountStores): () -> Unit {
        val registerCall = mockingDetails(accountStores).invocations
            .single { it.method.name == "registerCloser" }
        @Suppress("UNCHECKED_CAST")
        return registerCall.arguments[1] as () -> Unit
    }

    @Test
    fun `an offline switch re-seeds the incoming actor's persisted floor`() = runBlocking {
        val (store, state, accountStores) = makeSwitchable(initialActor = "actor-a", initialSnapshot = null)
        assertNull("actor A has no persisted snapshot at construction", store.inputs.value.contentPolicy)

        // `AccountSettingsVM.switchAccount` calls `registry.setActive(b)`
        // BEFORE `actorScope.dropActorScopedState()` runs the closer, so the
        // active pointer already names the incoming actor by the time this
        // fires — the exact ordering the closer's re-seed relies on.
        val closer = closerOf(accountStores)
        state.activeActor = "actor-b"
        state.snapshot = snapshotFor("collapse")
        closer.invoke()

        assertEquals(
            "an offline in-process switch must re-seed the INCOMING actor's " +
                "persisted floor, not leave it unsupervised until a read " +
                "succeeds (family-safety.md § Content policy, clause 2)",
            "collapse",
            store.inputs.value.contentPolicy?.nsfw,
        )
        assertEquals(true, store.inputs.value.contentNotify)
    }

    @Test
    fun `switching to an actor with no persisted snapshot leaves both halves blank`() = runBlocking {
        val (store, state, accountStores) = makeSwitchable(
            initialActor = "actor-a",
            initialSnapshot = snapshotFor("block"),
        )
        assertNotNull("actor A's restored floor must load at construction", store.inputs.value.contentPolicy)

        val closer = closerOf(accountStores)
        state.activeActor = "actor-b"
        state.snapshot = null
        closer.invoke()

        assertNull(
            "an unsupervised incoming actor must render unsupervised, not the " +
                "outgoing ward's own floor",
            store.inputs.value.contentPolicy,
        )
        assertEquals(false, store.inputs.value.contentNotify)
    }

    @Test
    fun `a same-actor drop does not resurrect the outgoing actor's own floor`() = runBlocking {
        val (store, _, accountStores) = makeSwitchable(
            initialActor = "actor-a",
            initialSnapshot = snapshotFor("block"),
        )
        assertNotNull("actor A's restored floor must load at construction", store.inputs.value.contentPolicy)

        // A debug `TestAgent` `reset`/`logout` arm whose credential wipe does
        // not reach the registry (`FileSecretBackend` e2e mode: `SecureStorage
        // .clear()` only empties the DIFFERENT `fauna_secure_prefs` file) —
        // the active pointer and its snapshot are UNCHANGED here, exactly
        // like that failure mode.
        val closer = closerOf(accountStores)
        closer.invoke()

        assertNull(
            "a same-actor drop must land blank, never resurrect the SAME " +
                "actor's own last-known floor under what is supposed to be a " +
                "blank reset",
            store.inputs.value.contentPolicy,
        )
    }
}
