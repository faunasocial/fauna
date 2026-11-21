package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.SavedStateHandle
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.WardAsks
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiSubscriptionStatus
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.ArgumentMatchers.any
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility
import org.mockito.Mockito.`when` as whenever

/**
 * `profile-follow-button` waits on the open-time status read (`profile.md`
 * § Errors & edge cases → *Unread state is not possible*; tui's
 * `follow_waits_for_the_status_read`): before it the button's answer is
 * blocked still loading and a press refuses with that reason on
 * `error-message`, sending nothing — so a press never subscribes again to an
 * author the viewer already follows; a failed read blocks it load-failed; an
 * answered one enables it. The answer is the live shared `readPossibility`
 * over UniFFI (`just android-host-test`).
 */
@ExperimentalCoroutinesApi
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProfileVMFollowReadTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val peer = "cd".repeat(32)
    private val context: Context = ApplicationProvider.getApplicationContext()

    private fun harness(status: () -> FfiSubscriptionStatus): Pair<ProfileVM, ApiClient> {
        val api = mock(ApiClient::class.java)
        val storage = mock(SecureStorage::class.java)
        whenever(api.wardAsks).thenReturn(WardAsks())
        whenever(storage.secretHex).thenReturn("11".repeat(32))
        runBlocking {
            whenever(api.subscriptionStatus(anyString())).thenAnswer { status() }
            whenever(api.subscriptionSubscribe(anyString(), anyString(), any())).thenAnswer { null }
        }
        return ProfileVM(SavedStateHandle(mapOf("actorId" to peer)), storage, api, context) to api
    }

    private fun subscribes(api: ApiClient): Int =
        mockingDetails(api).invocations.count { it.method.name == "subscriptionSubscribe" }

    private fun blocked(key: String) = Possibility.Blocked(LocalizedText(key, emptyMap()))

    @Test
    fun beforeTheStatusReadTheFollowIsBlockedAndAPressRefuses() {
        val (vm, api) = harness { FfiSubscriptionStatus(tier = null, expiresAt = null, autoApprove = false) }
        assertEquals(blocked("common.still_loading"), vm.followAnswer.value)
        vm.follow()
        assertEquals("Still loading — try again in a moment", vm.errorMessage.value)
        assertEquals(0, subscribes(api))
    }

    @Test
    fun aFailedStatusReadBlocksTheFollowWithLoadFailed() {
        val (vm, api) = harness { throw RuntimeException("offline") }
        vm.refreshFollowStatus()
        assertEquals(blocked("common.load_failed"), vm.followAnswer.value)
        vm.follow()
        assertEquals("Failed to load", vm.errorMessage.value)
        assertEquals(0, subscribes(api))
    }

    @Test
    fun anAnsweredStatusReadEnablesTheFollow() {
        val (vm, api) = harness { FfiSubscriptionStatus(tier = null, expiresAt = null, autoApprove = false) }
        vm.refreshFollowStatus()
        assertEquals(Possibility.Possible, vm.followAnswer.value)
        vm.follow()
        assertEquals(1, subscribes(api))
    }
}
