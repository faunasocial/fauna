package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.ApiClient
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiBridgeIdentity
import com.fauna.ffi.FfiBridgeStatus
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
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility
import org.mockito.Mockito.`when` as whenever

/**
 * `nostr-zap-signer-add-btn` waits on the `zaps` features read
 * (`docs/goal/ui/README.md` § Offer only what is possible, rule 5; tui's
 * `designate_waits_for_the_read_and_is_absent_without_the_plane`): before it
 * the gate is blocked still loading and a designate refuses with that reason
 * on `error-message`, sending nothing (rule 6); a read that threw blocks it
 * load-failed. The live shared gate over UniFFI (`just android-host-test`).
 */
@ExperimentalCoroutinesApi
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class NostrVMZapGateTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val context: Context = ApplicationProvider.getApplicationContext()

    private fun blocked(key: String) = Possibility.Blocked(LocalizedText(key, emptyMap()))

    private fun linked() = FfiBridgeStatus(
        id = "nostr",
        name = "Nostr",
        available = true,
        linked = true,
        identity = FfiBridgeIdentity(label = "Public Key", value = "npub1abc", display = "npub1abc"),
        mode = "generated",
        settings = emptyList(),
        supportsFollows = true,
        supportsFollowRequests = false,
        linkModes = null,
        error = null,
    )

    @Test
    fun beforeTheReadADesignateIsBlockedAndRefusesWithItsReason() {
        val api = mock(ApiClient::class.java)
        val vm = NostrVM(api, context)
        assertEquals(blocked("common.still_loading"), vm.zapSignerGate.value)
        vm.addZapSigner("ab".repeat(32), "Alby")
        assertEquals("Still loading — try again in a moment", vm.error.value)
        assertEquals(emptyList<String>(), mockingDetails(api).invocations.map { it.method.name })
    }

    @Test
    fun aFailedFeaturesReadBlocksTheDesignateWithLoadFailed() {
        val api = mock(ApiClient::class.java)
        runBlocking {
            whenever(api.listBridges()).thenReturn(listOf(linked()))
            whenever(api.fetchFeatures()).thenThrow(RuntimeException("offline"))
        }
        val vm = NostrVM(api, context)
        vm.refresh()
        assertEquals(blocked("common.load_failed"), vm.zapSignerGate.value)
        vm.addZapSigner("ab".repeat(32), "Alby")
        assertEquals("Failed to load", vm.error.value)
    }

    @Test
    fun aLoadedReadWithNoZapsRowIsAbsent() {
        val api = mock(ApiClient::class.java)
        runBlocking {
            whenever(api.listBridges()).thenReturn(listOf(linked()))
            whenever(api.fetchFeatures()).thenReturn(emptyList())
        }
        val vm = NostrVM(api, context)
        vm.refresh()
        assertEquals(Possibility.Absent, vm.zapSignerGate.value)
    }
}
