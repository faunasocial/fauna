package com.fauna.app.core.push

import android.content.Context
import org.unifiedpush.android.connector.UnifiedPush

/**
 * Where android gets a Web Push endpoint from: a UnifiedPush distributor
 * (`common.md` § Push Notifications → *Transports*, the android ruling —
 * UnifiedPush-first). The endpoint arrives asynchronously, in
 * [FaunaPushService.onNewEndpoint], never as this call's return.
 *
 * A seam so the e2e build can stand in for a distributor app the harness never
 * installs (`TestAgent.pushDistributor`); every shipping build uses
 * [UnifiedPushDistributor].
 */
interface PushDistributor {
    /** Whether a distributor is ready to hand out an endpoint — the saved one,
     *  or else the first one installed, which this call saves. `false` = this
     *  phone has none. */
    fun ensureDistributor(context: Context): Boolean

    /** Ask the distributor for this install's endpoint, signed for by the
     *  nest's VAPID key (RFC 8292). Answered later through [FaunaPushService]. */
    fun register(context: Context, vapidPublicKey: String)

    /** Withdraw this install's endpoint from the distributor. */
    fun unregister(context: Context)
}

/**
 * The UnifiedPush connector. Its own key pair and auth secret (RFC 8291) are
 * what the endpoint hands the nest, and it decrypts each message before
 * [FaunaPushService.onMessage] sees it — the part a browser's `PushManager`
 * plays on web.
 */
object UnifiedPushDistributor : PushDistributor {
    override fun ensureDistributor(context: Context): Boolean {
        if (UnifiedPush.getAckDistributor(context) != null) return true
        val pick = pickDistributor(
            saved = UnifiedPush.getSavedDistributor(context),
            installed = UnifiedPush.getDistributors(context),
            ownPackage = context.packageName,
        ) ?: return false
        UnifiedPush.saveDistributor(context, pick)
        return true
    }

    override fun register(context: Context, vapidPublicKey: String) {
        UnifiedPush.register(context, vapid = vapidPublicKey)
    }

    override fun unregister(context: Context) {
        UnifiedPush.unregister(context)
    }
}

/**
 * Which installed distributor to register with — UnifiedPush-first (`common.md`
 * § Push Notifications → *Transports*): a distributor app the user installed
 * always wins, the saved one first while it is still installed; this app's
 * own package — the embedded distributor, which lists the app among the
 * installed distributors on a Play build — only when no other is installed.
 * `null` = this phone has none.
 */
internal fun pickDistributor(saved: String?, installed: List<String>, ownPackage: String): String? {
    val userInstalled = installed.filter { it != ownPackage }
    return saved?.takeIf { it in userInstalled }
        ?: userInstalled.firstOrNull()
        ?: ownPackage.takeIf { it in installed }
}
