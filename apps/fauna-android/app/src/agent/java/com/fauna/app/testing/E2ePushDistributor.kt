package com.fauna.app.testing

import android.content.Context
import android.util.Base64
import com.fauna.app.core.push.FaunaPushService
import com.fauna.app.core.push.PushDistributor
import com.fauna.app.core.push.PushHostEntryPoint
import com.fauna.ffi.FfiPushEndpoint
import dagger.hilt.android.EntryPointAccessors
import java.security.KeyPairGenerator
import java.security.SecureRandom
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import kotlin.concurrent.thread

/**
 * The e2e build's stand-in for a UnifiedPush distributor app — debug source set
 * only (convention 15), chosen by [TestAgent.pushDistributor] under e2e. The
 * harness installs no distributor, so this answers [register] the way one
 * does: later, on another thread, through the same [FaunaPushService] path's
 * host callback — so everything after the endpoint (the toggle, the shared
 * registration machine, the nest row, the install bit) is the real one, as
 * web's `push_service_stand_in` leaves it.
 *
 * The endpoint is a loopback IP literal, the one `web-push` shape a test-hooks
 * nest accepts (`common.md` § Registration); port 9 refuses at once if a
 * dispatch ever dials it. The key pair is a real P-256 one, so the row is what
 * a distributor's would be.
 */
object E2ePushDistributor : PushDistributor {
    const val ENDPOINT = "http://127.0.0.1:9/push-stand-in"

    private val keys: Pair<String, String> by lazy {
        val gen = KeyPairGenerator.getInstance("EC").apply { initialize(ECGenParameterSpec("secp256r1")) }
        val point = (gen.generateKeyPair().public as ECPublicKey).w
        val uncompressed = byteArrayOf(0x04) + fixed32(point.affineX.toByteArray()) + fixed32(point.affineY.toByteArray())
        val auth = ByteArray(16).also { SecureRandom().nextBytes(it) }
        b64url(uncompressed) to b64url(auth)
    }

    override fun ensureDistributor(context: Context): Boolean = true

    override fun register(context: Context, vapidPublicKey: String) {
        val host = EntryPointAccessors
            .fromApplication(context.applicationContext, PushHostEntryPoint::class.java)
            .pushHost()
        val (p256dh, auth) = keys
        thread(name = "e2e-push-distributor") {
            host.onEndpoint(FfiPushEndpoint(FaunaPushService.WEB_PUSH, ENDPOINT, p256dh, auth))
        }
    }

    override fun unregister(context: Context) = Unit

    /** A BigInteger's bytes as exactly 32 (a sign byte dropped, short ones padded). */
    private fun fixed32(b: ByteArray): ByteArray =
        if (b.size >= 32) b.copyOfRange(b.size - 32, b.size) else ByteArray(32 - b.size) + b

    private fun b64url(b: ByteArray): String =
        Base64.encodeToString(b, Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP)
}
