package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.SavedStateHandle
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.WardAsks
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.`when` as whenever

/**
 * [ProfileVM]'s `profile-block-button` edge state (`profile.md` § User actions):
 * the toggle is inert until the open-time `contacts.list` read lands, and a late
 * read never rewinds the viewer's own press — tui's
 * `the_toggle_is_inert_until_the_open_time_read_lands` and
 * `a_late_open_hydration_does_not_rewind_the_user_toggle`, driven through the VM.
 */
@ExperimentalCoroutinesApi
class ProfileVMBlockTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val peer = "cd".repeat(32)

    private fun vm(api: ApiClient): ProfileVM {
        val storage = mock(SecureStorage::class.java)
        val context = mock(Context::class.java)
        whenever(api.wardAsks).thenReturn(WardAsks())
        whenever(storage.secretHex).thenReturn("11".repeat(32))
        return ProfileVM(SavedStateHandle(mapOf("actorId" to peer)), storage, api, context)
    }

    private fun blockCalls(api: ApiClient): Int =
        mockingDetails(api).invocations.count { it.method.name == "blockKnock" || it.method.name == "unblockKnock" }

    @Test
    fun theToggleIsInertUntilTheOpenTimeReadLands() {
        val api = mock(ApiClient::class.java)
        val vm = vm(api)

        assertFalse("the edge is not yet known at open", vm.blockKnown.value)
        vm.toggleBlock()
        assertEquals0("a press before the read lands issues nothing", blockCalls(api))

        vm.applyBlockRead(true)
        assertTrue(vm.blockKnown.value)
        assertTrue("the read says blocked", vm.blocked.value)
    }

    @Test
    fun aLateReadDoesNotRewindTheViewersPress() {
        val api = mock(ApiClient::class.java)
        val vm = vm(api)
        vm.applyBlockRead(false)

        vm.toggleBlock() // blocks
        assertTrue(vm.blocked.value)

        // The open-time read, issued before the press, finally returns not-blocked.
        vm.applyBlockRead(false)
        assertTrue("a read issued before the press must not rewind it", vm.blocked.value)
    }

    private fun assertEquals0(message: String, actual: Int) =
        org.junit.Assert.assertEquals(message, 0, actual)
}
