package com.fauna.app.core

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.viewmodel.SupervisedIndicatorVM
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.FfiContentPolicy
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
import org.junit.Assert.assertNotNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 *  `AccountSettingsVM.removeAccount` (`AccountSettingsVM.kt:182-196`)
 * is offered only for a non-active account, and calls
 * `AccountStores.eraseAccount` (`AccountStores.kt:604-617`), whose first
 * statement used to be an unconditional `closeOpenStores()` — which runs EVERY
 * registered identity closer, not just the erased actor's. `ContentPolicyStore`,
 * `ScreenTimeStore` and `SupervisedIndicatorVM` each guard their re-seed on
 * `newActor != seededActorHex`, but the pre-guard blank ran regardless — so
 * erasing a DIFFERENT, non-active account blanked the ACTIVE ward's guardian
 * floor, bedtime lock and indicator, and the same-actor guard then re-seeded
 * nothing, exactly like the debug reset/logout same-actor-drop case
 * ([ContentPolicyStoreTest], [ScreenTimeStoreTest],
 * [com.fauna.app.ui.viewmodel.SupervisedIndicatorVMTest]).
 *
 * `family-client-enforcement.md` clause 1 (`:98`, a failed
 * `fauna.family.status` read must never move enforcement state) and clause 2
 * (`:99`, the persisted snapshot restores ahead of the first read) both bind
 * here: this test is offline throughout (`familyStatus()` always throws), so
 * the persisted snapshot restored at construction is the ONLY thing keeping
 * the active ward supervised, and `account-scoping.md:248-251` backs the fix
 * this pins — `removeAccount`/`eraseAccount` "targets a non-active actor" and
 * "erases no live engine", so the identity closers (which describe the
 * ACTIVE session) must not run at all for that erase.
 *
 * Real `AccountStores` over a real `FfiAccountRegistry`, with the three real
 * closer-registering classes wired to it — the interaction the per-class
 * mocked-`AccountStores` suites ([ContentPolicyStoreTest], [ScreenTimeStoreTest],
 * [SupervisedIndicatorVMTest]) cannot see, since they mock `AccountStores`
 * away and drive the captured closer lambda directly. `ApiClient` stays a
 * mock — no wire traffic. `accountStores.database()` is never called, so no
 * Room `SQLiteDatabase` is ever opened: `AccountStores.closeOpenStores`'s
 * `db?.close()` is a no-op against a null handle, `eraseAccount`'s FFI erase
 * call is plain file-system work, and the supervision snapshot round trip is
 * the `SecretStore` seam, not SQLite — Robolectric's real-SQLite ARM64 gap
 * ([AccountStoresEraseTest.anEraseYieldsAFreshHandleAndRepointsTheStaticAccessor])
 * is never reached.
 */
@ExperimentalCoroutinesApi
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class EraseNonActiveAccountKeepsActiveWardEnforcedTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun supervisedFamilyStatus(): FfiFamilyStatus {
        val guardian = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 9 }, handle = "guardian")
        val floor = FfiContentPolicy(nsfw = "block", spam = "inherit", phishing = "inherit", commercial = "inherit")
        // Locked all day regardless of the real wall clock (testing.md
        // convention 14) — mirrors ScreenTimeStoreTest.alwaysLockedWindow.
        val window = FfiScreenTimePolicy(windowStart = 0u, windowEnd = 0u, dailyMinutes = null)
        return FfiFamilyStatus(
            supervisedBy = guardian,
            policy = FfiReachPolicy(
                contactApproval = false,
                unknownSenderMail = "allow",
                federationContact = true,
                feedSources = "allow",
                contentPolicy = floor,
                screenTime = window,
                contentNotify = true,
                unknownPeerDm = null,
            ),
            wards = emptyList(),
            incomingTransfers = emptyList(),
            usageTodayMinutes = null,
            contactRequests = emptyList(),
            feedRequests = emptyList(),
            ageBand = null,
            // The gated fold shared Rust attaches to a real supervised reply.
            supervision = FfiSupervisionSnapshot(
                supervisedBy = guardian,
                contentPolicy = floor,
                contentNotify = true,
                screenTime = window,
            ),
        )
    }

    @Test
    fun `erasing a non-active account leaves the active ward's floor, lock and indicator intact`() = runBlocking {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
        // The FIRST added account auto-activates (AccountSettingsVM.kt's own
        // doc on `addAccount`); the second stays non-active — exactly the row
        // this pins, offered only for a non-active row
        // (AccountSwitcherSection.kt:112-122).
        val activeActor = registry.addAccount(SECRET_A, null, null)
        val nonActiveActor = registry.addAccount(SECRET_B, null, null)
        val accountStores = AccountStores(ApplicationProvider.getApplicationContext(), registry)
        assertEquals(activeActor, accountStores.activeActorHex())

        // set_supervision_snapshot_json no-ops for an actor absent from the
        // index (fauna-client-accounts/src/lib.rs), so persisting AFTER
        // add_account is required, not incidental.
        accountStores.persistSupervisionSnapshot(activeActor, supervisedFamilyStatus())

        val api = mock(ApiClient::class.java)
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        // Offline throughout: the restored snapshot is the only source for
        // every assertion below (family-client-enforcement.md:98).
        whenever(api.familyStatus()).thenAnswer { throw RuntimeException("offline") }
        whenever(api.getSpamPreferences()).thenAnswer { throw RuntimeException("offline") }

        val contentPolicyStore = ContentPolicyStore(api, accountStores)
        val screenTimeStore = ScreenTimeStore(ApplicationProvider.getApplicationContext(), api, accountStores)
        val supervisedIndicatorVM = SupervisedIndicatorVM(api, screenTimeStore, accountStores)

        assertNotNull(
            "precondition: the active ward's floor restores from the persisted snapshot",
            contentPolicyStore.inputs.value.contentPolicy,
        )
        assertNotNull(
            "precondition: the active ward's bedtime lock restores from the persisted snapshot",
            screenTimeStore.lockMessage.value,
        )
        assertEquals(
            "precondition: the active ward's indicator restores from the persisted snapshot",
            "guardian",
            supervisedIndicatorVM.supervisedByHandle.value,
        )

        // The finding: erasing the OTHER, non-active account.
        accountStores.eraseAccount(nonActiveActor)

        assertNotNull(
            "erasing a non-active account must not blank the ACTIVE ward's " +
                "content floor — the identity closers describe the active " +
                "session, which did not change (family-client-enforcement.md:98-99)",
            contentPolicyStore.inputs.value.contentPolicy,
        )
        assertNotNull(
            "erasing a non-active account must not blank the ACTIVE ward's " +
                "bedtime lock",
            screenTimeStore.lockMessage.value,
        )
        assertEquals(
            "erasing a non-active account must not blank the ACTIVE ward's " +
                "supervised indicator",
            "guardian",
            supervisedIndicatorVM.supervisedByHandle.value,
        )
    }

    private companion object {
        const val SECRET_A =
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
        const val SECRET_B =
            "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f"
    }
}
