package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.SavedStateHandle
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.WardAsks
import com.fauna.ffi.FfiProfileDisplay
import com.fauna.ffi.FfiProfileHeaderRead
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.mockito.ArgumentMatchers.any
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.mock
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * The Profile header's stored pictures (`profile-avatar` / `profile-banner`,
 * `profile.md` § Layout & flow): rendered only while the profile names the
 * picture, empty until its bytes arrive, and fetched from the nest that served
 * the profile (`relayedFrom`) — tui's `the_header_paints_only_the_pictures_the_profile_names`
 * and `the_pictures_come_from_the_nest_that_served_the_profile`.
 */
@ExperimentalCoroutinesApi
class ProfileVMHeaderPicturesTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val peer = "cd".repeat(32)
    private val home = "https://home.example"
    private val homeId = "ab".repeat(32)
    private val body = byteArrayOf(1, 2, 3)

    private fun display(avatar: String?, banner: String?) =
        FfiProfileDisplay(
            displayName = "Ada",
            bio = null,
            links = emptyList(),
            avatarHashHex = avatar,
            bannerHashHex = banner,
            audienceEveryone = true,
        )

    private fun vm(api: ApiClient): ProfileVM {
        val storage = mock(SecureStorage::class.java)
        whenever(api.wardAsks).thenReturn(WardAsks())
        whenever(api.nodeUrl).thenReturn("https://own.example")
        whenever(storage.secretHex).thenReturn("11".repeat(32))
        return ProfileVM(SavedStateHandle(mapOf("actorId" to peer)), storage, api, mock(Context::class.java))
    }

    @Test
    fun theHeaderRendersOnlyThePictureTheProfileNames() {
        val api = mock(ApiClient::class.java)
        val fetched = byteArrayOf(9, 9)
        runBlocking {
            whenever(api.profileHeaderRead(anyString(), any())).thenReturn(FfiProfileHeaderRead(body, null, null))
            whenever(api.fetchProfilePicture(any(), any(), anyString())).thenReturn(fetched)
        }
        whenever(api.decodeProfileDisplay(body)).thenReturn(display("aa", null))
        val vm = vm(api)
        vm.refreshHeader()

        assertNull("an unnamed banner renders no element", vm.banner.value)
        val avatar = vm.avatar.value
        assertNotNull("a named avatar renders", avatar)
        assertEquals("aa", avatar!!.hashHex)
        assertArrayEquals(fetched, avatar.bytes)
    }

    @Test
    fun aFailedFetchLeavesThePictureEmptyNeverAnError() {
        val api = mock(ApiClient::class.java)
        runBlocking {
            whenever(api.profileHeaderRead(anyString(), any())).thenReturn(FfiProfileHeaderRead(body, null, null))
            whenever(api.fetchProfilePicture(any(), any(), anyString())).thenReturn(null)
        }
        whenever(api.decodeProfileDisplay(body)).thenReturn(display(null, "bb"))
        val vm = vm(api)
        vm.refreshHeader()

        val banner = vm.banner.value
        assertNotNull(banner)
        assertNull("empty after a failed fetch", banner!!.bytes)
        assertNull(vm.errorMessage.value)
    }

    @Test
    fun thePicturesComeFromTheNestThatServedTheProfile() {
        val api = mock(ApiClient::class.java)
        runBlocking {
            whenever(api.profileHeaderRead(anyString(), any())).thenReturn(FfiProfileHeaderRead(body, home, homeId))
            whenever(api.fetchProfilePicture(any(), any(), anyString())).thenReturn(byteArrayOf(7))
        }
        whenever(api.decodeProfileDisplay(body)).thenReturn(display("aa", "bb"))
        val vm = vm(api)
        vm.refreshHeader()

        runBlocking {
            verify(api).fetchProfilePicture(home, homeId, "aa")
            verify(api).fetchProfilePicture(home, homeId, "bb")
        }
        assertNotNull(vm.avatar.value?.bytes)
        assertNotNull(vm.banner.value?.bytes)
    }
}
