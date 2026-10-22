package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.SavedStateHandle
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.WardAsks
import com.fauna.app.ui.navigation.decodeProfileHome
import com.fauna.app.ui.navigation.profileRoute
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.mockito.ArgumentMatchers.any
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.`when` as whenever

/**
 * The profile page's **actor-home slot** (`profile.md` § Where logic lives → *The
 * actor's home*): seeded at open from the home the opener carried (route arg
 * `home`, [profileRoute]) through the shared `actorHome`, read by the follow, the
 * knock and the published `data.profile.home_nest_url`; a hint naming the viewer's
 * own nest is this nest. `actorHome` is the real shared Rust over UniFFI, so this
 * runs under `just android-host-test`.
 */
@ExperimentalCoroutinesApi
class ProfileVMActorHomeTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val peer = "cd".repeat(32)
    private val own = "https://own.example"
    private val remote = "https://peer.example:9443"

    private class Harness(val vm: ProfileVM, val api: ApiClient)

    private fun harness(home: String?): Harness {
        val api = mock(ApiClient::class.java)
        val storage = mock(SecureStorage::class.java)
        val context = mock(Context::class.java)
        whenever(api.wardAsks).thenReturn(WardAsks())
        whenever(api.nodeUrl).thenReturn(own)
        whenever(storage.secretHex).thenReturn("11".repeat(32))
        runBlocking {
            whenever(api.subscriptionSubscribe(anyString(), anyString(), any())).thenAnswer { null }
            whenever(api.sendKnock(anyString(), anyString(), any())).thenAnswer { null }
        }
        val args = buildMap<String, Any?> {
            put("actorId", peer)
            home?.let { put("home", profileRoute(peer, it).substringAfter("?home=")) }
        }
        return Harness(ProfileVM(SavedStateHandle(args), storage, api, context), api)
    }

    @Test
    fun theCarriedHomeSeedsTheSlotAndIsPublishedForTheStateProtocol() {
        val h = harness(remote)
        assertEquals(remote, h.vm.actorHomeSlot.value)
        val viewed = ProfileTestState.viewed
        assertNotNull(viewed)
        assertEquals(peer, viewed!!.actorId)
        assertEquals(remote, viewed.homeNestUrl)
    }

    @Test
    fun aHintNamingTheViewersOwnNestIsThisNest() {
        assertNull(harness(own).vm.actorHomeSlot.value)
    }

    @Test
    fun noCarriedHomeLeavesTheSlotOnThisNest() {
        assertNull(harness(null).vm.actorHomeSlot.value)
    }

    @Test
    fun theFollowCarriesTheSlot() {
        val h = harness(remote)
        h.vm.follow()
        val follows = mockingDetails(h.api).invocations
            .filter { it.method.name == "subscriptionSubscribe" }
            .map { it.arguments.take(3) }
        assertEquals(listOf(listOf<Any?>(peer, ProfileOffersVM.FOLLOWERS_TIER, remote)), follows)
    }

    @Test
    fun theKnockRoutesToTheSlot() {
        val h = harness(remote)
        h.vm.requestContact()
        val routes = mockingDetails(h.api).invocations
            .filter { it.method.name == "sendKnock" }
            .map { it.arguments[2] }
        assertEquals(listOf<Any?>(remote), routes)
    }

    @Test
    fun theRouteArgRoundTripsAndAnAbsentOrMalformedOneIsNoHome() {
        assertEquals("profile/$peer", profileRoute(peer, null))
        assertEquals("profile/$peer", profileRoute(peer, "  "))
        val arg = profileRoute(peer, remote).substringAfter("?home=")
        assertEquals(remote, decodeProfileHome(arg))
        assertNull(decodeProfileHome(null))
        assertNull(decodeProfileHome(""))
        assertNull(decodeProfileHome("abc"))
        assertNull(decodeProfileHome("zz"))
    }
}
