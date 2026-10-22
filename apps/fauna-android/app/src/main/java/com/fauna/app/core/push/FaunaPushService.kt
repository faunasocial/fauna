package com.fauna.app.core.push

import com.fauna.app.core.ShellLog
import com.fauna.ffi.FfiPushEndpoint
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.android.EntryPointAccessors
import dagger.hilt.components.SingletonComponent
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage

/** How the connector's service reaches the app's one [PushHost]. */
@EntryPoint
@InstallIn(SingletonComponent::class)
interface PushHostEntryPoint {
    fun pushHost(): PushHost
}

/**
 * The UnifiedPush connector's callbacks (declared in the manifest under the
 * connector's `PUSH_EVENT` action). Each hands straight to [PushHost]: an
 * endpoint becomes a `web-push` subscription with the connector's own key pair
 * and auth secret, a message arrives already decrypted (RFC 8291).
 */
class FaunaPushService : PushService() {
    private val host: PushHost by lazy {
        EntryPointAccessors.fromApplication(applicationContext, PushHostEntryPoint::class.java).pushHost()
    }

    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        val keys = endpoint.pubKeySet
        if (keys == null) {
            // Without the keys the nest cannot encrypt to this phone, and the
            // nest only sends encrypted payloads.
            host.onEndpointRefused("the distributor gave an endpoint without keys")
            return
        }
        host.onEndpoint(FfiPushEndpoint(WEB_PUSH, endpoint.url, keys.pubKey, keys.auth))
    }

    override fun onMessage(message: PushMessage, instance: String) {
        if (!message.decrypted) {
            ShellLog.w("Push", "dropped a push message the connector could not decrypt")
            return
        }
        host.onMessage(message.content)
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        host.onEndpointRefused(reason.name)
    }

    override fun onUnregistered(instance: String) {
        // The distributor dropped this install (uninstalled, or the user
        // removed it there). The nest cleans the dead row on its next dial's
        // 404/410; the bit stays as the user set it.
        ShellLog.w("Push", "the UnifiedPush distributor unregistered this install")
    }

    companion object {
        const val WEB_PUSH = "web-push"
    }
}
