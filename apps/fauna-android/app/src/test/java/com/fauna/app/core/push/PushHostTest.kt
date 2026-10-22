package com.fauna.app.core.push

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.NotificationHelper
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiNestClient
import com.fauna.ffi.FfiPushEndpoint
import com.fauna.ffi.FfiPushIntent
import com.fauna.ffi.FfiPushRegistration
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.timeout
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * [PushHost]'s routing (`android.md` § Push Notifications): a leave hands the
 * leaving registration to the teardown that drops its row, a same-identity
 * rebuild drops nothing, and an endpoint nobody is waiting for re-arms.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class PushHostTest {
    private val reg: FfiPushRegistration = mock(FfiPushRegistration::class.java)
    private val nest: FfiNestClient = mock(FfiNestClient::class.java)

    private fun signedIn(optedIn: Boolean = false): PushHost {
        val host = PushHost(ApplicationProvider.getApplicationContext(), mock(NotificationHelper::class.java))
        whenever(nest.pushRegistration(host.intentPath, "actor", "device")).thenReturn(reg)
        whenever(reg.intent()).thenReturn(FfiPushIntent(optedIn, null))
        runBlocking { host.onSessionStart(nest, "actor", "device") }
        return host
    }

    @Test
    fun aSessionStartAnnouncesThisDevice() {
        signedIn()
        runBlocking { verify(reg).announcePresence() }
    }

    @Test
    fun aLeaveHandsTheLeavingRegistrationToTheTeardown() {
        val host = signedIn()
        assertSame(reg, host.onSessionEnd(identityEnds = true))
        // Gone with the session: a second teardown has nothing to drop.
        assertNull(host.onSessionEnd(identityEnds = true))
    }

    @Test
    fun aSameIdentityRebuildDropsNothing() {
        assertNull(signedIn().onSessionEnd(identityEnds = false))
    }

    @Test
    fun anEndpointNobodyWaitsForReArms() {
        val host = signedIn()
        val endpoint = FfiPushEndpoint("web-push", "https://push.example/up/1", "pk", "auth")
        host.onEndpoint(endpoint)
        runBlocking { verify(reg, timeout(5_000)).rearm(endpoint) }
    }

    @Test
    fun noSessionNoEnable() {
        val host = PushHost(ApplicationProvider.getApplicationContext(), mock(NotificationHelper::class.java))
        assertSame(PushHost.EnableFailure.NoSession, runBlocking { host.enable() })
    }
}
