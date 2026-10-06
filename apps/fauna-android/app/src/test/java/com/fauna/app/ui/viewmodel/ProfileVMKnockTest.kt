package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.SavedStateHandle
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ContactAskRender
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.WardAsks
import com.fauna.ffi.FfiException
import com.fauna.ffi.FfiFamilyContactRequest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.ArgumentMatchers.any
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.`when` as whenever

/**
 * [ProfileVM]'s `profile-request-contact-button` knock and the ward's
 * guardian-ask pair (family-safety.md § Child-initiated contact requests →
 * *App affordance*) — tui's `profile/mod.rs` arms, driven through the VM:
 * the knock goes to the VIEWED actor, only the TYPED refusal offers the ask
 * and it stays on `error-message` (rules (a), (b)), and the landed ask reads
 * pending.
 *
 * Collaborators are mocks; `actorIdFromSecret` is the real shared Rust over
 * UniFFI, so this runs under `just android-host-test`.
 */
@ExperimentalCoroutinesApi
class ProfileVMKnockTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val peer = "cd".repeat(32)
    private val guardianSentence = "This account can only message approved contacts."

    private class Harness(val vm: ProfileVM, val api: ApiClient, val wardAsks: WardAsks)

    private fun harness(knock: () -> Unit): Harness {
        val api = mock(ApiClient::class.java)
        val storage = mock(SecureStorage::class.java)
        val context = mock(Context::class.java)
        val wardAsks = WardAsks()
        whenever(api.wardAsks).thenReturn(wardAsks)
        whenever(storage.secretHex).thenReturn("11".repeat(32))
        whenever(context.getString(R.string.contacts_guardian_approval_required)).thenReturn(guardianSentence)
        runBlocking {
            // Mockito trims a suspend function's continuation, so three
            // matchers for its three declared parameters.
            whenever(api.sendKnock(anyString(), anyString(), any()))
                .thenAnswer { knock() }
        }
        val vm = ProfileVM(SavedStateHandle(mapOf("actorId" to peer)), storage, api, context)
        return Harness(vm, api, wardAsks)
    }

    /** The `(recipient, route)` of every knock the VM issued. */
    private fun knocks(api: ApiClient): List<Pair<Any?, Any?>> =
        mockingDetails(api).invocations
            .filter { it.method.name == "sendKnock" }
            .map { it.arguments[1] to it.arguments[2] }

    @Test
    fun theKnockGoesToTheViewedActor_andASentKnockIsNotReSent() {
        val h = harness { }
        h.vm.requestContact()
        // No open-time profile read landed → no better route than this nest.
        assertEquals(listOf<Pair<Any?, Any?>>(peer to null), knocks(h.api))
        assertTrue(h.vm.knockAsk.value.knockSent)
        assertNull(h.vm.errorMessage.value)

        h.vm.requestContact()
        assertEquals("a sent knock is not re-sent", 1, knocks(h.api).size)
    }

    @Test
    fun aGuardianRefusedKnockOffersTheAskAndThenShowsItPending() {
        val h = harness { throw FfiException.GuardianApprovalRequired("approval required") }
        assertNull("nothing paints before a refusal", h.vm.contactAskRenderFor(h.vm.knockAsk.value))

        h.vm.requestContact()
        assertEquals(ContactAskRender.ASK, h.vm.contactAskRenderFor(h.vm.knockAsk.value))
        assertFalse(h.vm.knockAsk.value.knockSent)
        assertEquals("the refusal stays on error-message", guardianSentence, h.vm.errorMessage.value)

        runBlocking {
            whenever(h.api.familyContactRequest(peer)).thenAnswer {
                // The nest's re-read lands in the durable store (ApiClient's job).
                h.wardAsks.replaceContactRequests(
                    listOf(FfiFamilyContactRequest(peerActorId = ByteArray(32) { 0xcd.toByte() }, peerHandle = "", createdAt = 0)),
                )
            }
        }
        h.vm.askGuardian()
        assertEquals(ContactAskRender.PENDING, h.vm.contactAskRenderFor(h.vm.knockAsk.value))
        assertNull("the landed ask retires the refusal", h.vm.errorMessage.value)
        assertTrue(h.wardAsks.contactAskPending(peer))
    }

    /** Rule (a): a plain failure — even one whose text names the gate — offers
     *  nothing, and shows its own text. */
    @Test
    fun aTransportFailureDoesNotImplySupervision() {
        val h = harness { throw IllegalStateException("inbox send: guardian_approval_required?") }
        h.vm.requestContact()
        assertNull(h.vm.contactAskRenderFor(h.vm.knockAsk.value))
        assertEquals("inbox send: guardian_approval_required?", h.vm.errorMessage.value)
        assertFalse("a failed knock can be retried", h.vm.knockAsk.value.knockInFlight)
    }
}
