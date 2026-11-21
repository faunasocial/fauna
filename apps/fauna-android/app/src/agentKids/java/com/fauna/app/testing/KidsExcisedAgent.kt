package com.fauna.app.testing

import android.content.Context
import org.json.JSONObject

/**
 * The agent arms that drive a plane the kids build excises — **the refusing
 * half** (`src/agentKids`, taken by `kidsDebug` only), signature for signature
 * with `src/agentNoKids`' built twin. The kids bindings carry none of these
 * planes, so every command routed here ([KidsExcisedActions.PLANES]) is
 * refused by its plane's name — never acked (convention 11) — and every state
 * key the built twin publishes is ABSENT here (or `null` where the shared
 * shape reserves `null` for "no leg"), never a zero: the kids app has no such
 * leg.
 */
object KidsExcisedAgent {
    /** Nothing to resolve: the kids graph binds none of the excised hosts. */
    @Suppress("UNUSED_PARAMETER")
    fun start(context: Context) = Unit

    @Suppress("UNUSED_PARAMETER", "RedundantSuspendModifier")
    suspend fun dispatch(action: String, cmd: JSONObject): String? = KidsExcisedActions.refusal(action)

    /** No feed: `data.feed.posts` reads empty. */
    @Suppress("UNUSED_PARAMETER")
    fun feedPostsRaw(api: com.fauna.app.core.ApiClient?): String? = null

    /** No feed leg: `null`, the shared shape's "this app publishes no leg". */
    @Suppress("UNUSED_PARAMETER")
    fun feedReloads(api: com.fauna.app.core.ApiClient?): Any = JSONObject.NULL

    /** No rule editor in the kids bindings: the key is absent. */
    @Suppress("UNUSED_PARAMETER")
    fun putWardRuleDocumentRules(data: JSONObject, store: com.fauna.app.core.ContentPolicyStore?) = Unit

    /** No share plane: the key is absent. */
    @Suppress("UNUSED_PARAMETER")
    fun putShareServeTally(data: JSONObject) = Unit

    /** No critical-alerts sweep: the key is absent. */
    @Suppress("UNUSED_PARAMETER")
    fun putAlertSweepPasses(state: JSONObject) = Unit

    fun alertSweepWake(action: String): String? =
        KidsExcisedActions.refusal(action, KidsExcisedActions.CRITICAL_ALERTS)

    /** No feed composer: the `compose-file` target is refused by its plane. */
    fun attachFeedComposeFile(path: String): String? =
        "compose.file[compose-file]: the ${KidsExcisedActions.FEED} plane is excised from the kids build " +
            "(asked to stage $path)"
}
