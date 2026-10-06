package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.ContentPolicyStore
import com.fauna.app.core.ScreenTimeStore
import com.fauna.ffi.FfiContentPolicy
import com.fauna.ffi.FfiFamilyGuardianInfo
import com.fauna.ffi.FfiFamilyStatus
import com.fauna.ffi.FfiReachPolicy
import com.fauna.ffi.FfiScreenTimePolicy
import com.fauna.ffi.FfiSupervisionSnapshot
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * [FamilyVM]'s page read feeds the ward's client-enforced inputs — the global
 * `screen-time-lock`, the content floor and Guardian Notify — off the reply's
 * gated supervision fold (`FfiFamilyStatus.supervision`), never the raw
 * `policy` (family-client-enforcement.md § Implementation status today; web's
 * Family page is the reference shape).
 *
 * FFI-free: every collaborator is a mock, and `refresh` launches on
 * `viewModelScope`, so the Main dispatcher is the unconfined test one, as in
 * [SupervisedIndicatorVMTest].
 */
@ExperimentalCoroutinesApi
class FamilyVMTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val guardian = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 6 }, handle = "guardian")
    private val floor = FfiContentPolicy(nsfw = "block", spam = "inherit", phishing = "inherit", commercial = "inherit")
    private val window = FfiScreenTimePolicy(windowStart = 1260u, windowEnd = 420u, dailyMinutes = 90u)

    /** A successful reply whose policy sets all three inputs; whether the gated
     *  fold carries them is decided by [supervisedBy], as shared Rust's is. */
    private fun status(supervisedBy: FfiFamilyGuardianInfo?) = FfiFamilyStatus(
        supervisedBy = supervisedBy,
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
        usageTodayMinutes = 12u,
        contactRequests = emptyList(),
        feedRequests = emptyList(),
        ageBand = null,
        supervision = supervisedBy?.let {
            FfiSupervisionSnapshot(supervisedBy = it, contentPolicy = floor, contentNotify = true, screenTime = window)
        },
    )

    private fun refreshWith(reply: FfiFamilyStatus): Pair<ScreenTimeStore, ContentPolicyStore> {
        val api = mock(ApiClient::class.java)
        val screenTimeStore = mock(ScreenTimeStore::class.java)
        val contentPolicyStore = mock(ContentPolicyStore::class.java)
        runBlocking {
            whenever(api.familyStatus()).thenReturn(reply)
            whenever(api.familyApprovalsList()).thenReturn(emptyList())
        }
        FamilyVM(api, screenTimeStore, contentPolicyStore).refresh()
        return screenTimeStore to contentPolicyStore
    }

    @Test
    fun `a page read under a guardian feeds the lock and the floor from the fold`() {
        val reply = status(supervisedBy = guardian)
        val (screenTimeStore, contentPolicyStore) = refreshWith(reply)

        verify(screenTimeStore).setWardScreenTime(window, "guardian", 12u)
        verify(contentPolicyStore).applySupervision(reply.supervision)
    }

    /** The un-deny (family-safety.md § The bridge-DM gate → *The un-deny
     *  surface*) is the shared `allowBlockedPeer` over the row's own ward and
     *  peer record — never the raw decide — and the page re-reads so the row
     *  drops from nest-confirmed state. */
    @Test
    fun `allowing a denied peer un-denies that row's own peer`() {
        val api = mock(ApiClient::class.java)
        runBlocking {
            whenever(api.familyStatus()).thenReturn(status(supervisedBy = null))
            whenever(api.familyApprovalsList()).thenReturn(emptyList())
        }
        val ward = ByteArray(32) { 9 }
        FamilyVM(api, mock(ScreenTimeStore::class.java), mock(ContentPolicyStore::class.java))
            .allowBlockedPeer(
                com.fauna.app.core.UndenyDecide(
                    ward,
                    com.fauna.ffi.FfiFamilyBlockedPeer(bridgeId = "nostr", peerId = "npub1bbb"),
                ),
            )

        val invocations = org.mockito.Mockito.mockingDetails(api).invocations
        val allow = invocations.single { it.method.name == "familyAllowBlockedPeer" }.arguments
        assertArrayEquals(ward, allow[0] as ByteArray)
        assertEquals(com.fauna.ffi.FfiFamilyBlockedPeer(bridgeId = "nostr", peerId = "npub1bbb"), allow[1])
        assertEquals(
            "the un-deny never spells the raw decide",
            0,
            invocations.count { it.method.name == "familyApprovalsDecide" },
        )
        runBlocking { verify(api).familyStatus() }
    }

    @Test
    fun `a page read whose policy names no guardian feeds nothing enforceable`() {
        val (screenTimeStore, contentPolicyStore) = refreshWith(status(supervisedBy = null))

        // The raw document still names a window and a floor; only the gated
        // fold keeps them off an unsupervised viewer.
        verify(screenTimeStore).setWardScreenTime(null, null, 12u)
        verify(contentPolicyStore).applySupervision(null)
    }
}
