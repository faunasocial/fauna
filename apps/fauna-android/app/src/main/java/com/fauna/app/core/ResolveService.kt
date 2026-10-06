package com.fauna.app.core

import com.fauna.ffi.classifyRecipient
import com.fauna.ffi.resolveHandle
import com.fauna.ffi.resolveNest
import javax.inject.Inject
import javax.inject.Singleton

@Singleton
class ResolveService @Inject constructor(
    private val api: ApiClient
) {
    data class ResolvedRecipient(
        val actorId: String,
        val nodeUrl: String,
        val handle: String? = null,
        val domain: String? = null
    )

    /**
     * Resolve a typed compose / find-user recipient. Parsing *and* the two
     * network lookups all run in shared Rust over UniFFI (`com.fauna.ffi`): the
     * `classify_recipient` split, then the anonymous `fauna.nest.resolve` /
     * `fauna.actor.by_handle` discovery kinds. No client-side actor-id regex,
     * `@`-split, or HTTP to the deleted `/api/v1/{resolve-node,actor/by-handle}`
     * twins remains here (priority #2/#4).
     */
    suspend fun resolve(input: String): ResolvedRecipient {
        // [kind, actorId, user, domain]; kind ∈ {actor_id, handle, invalid}.
        val parts = classifyRecipient(input.trim())
        return when (parts[0]) {
            "actor_id" -> ResolvedRecipient(
                actorId = parts[1],
                nodeUrl = api.nodeUrl
            )
            "handle" -> {
                // The home nest performs the SRV lookup; the resolved URL is the
                // nest that owns the handle (local nest or a cross-nest peer).
                val targetNodeUrl = resolveNest(api.nodeUrl, parts[3])
                // [actorId, handle, domain] — thread the typed `@domain` qualifier
                // (parts[3]) so a multi-domain handle echoes it and the find-result
                // renders `bob@domain2` (mail-multidomain.md § Multi-domain handles
                // § Resolution); `ContactsScreen` already shows `handle@domain`.
                val resolved = resolveHandle(targetNodeUrl, parts[2], parts[3])
                ResolvedRecipient(
                    actorId = resolved[0],
                    nodeUrl = targetNodeUrl,
                    handle = resolved[1],
                    domain = resolved[2]
                )
            }
            else -> throw IllegalArgumentException("Enter a handle (alice@fauna.social) or 64-char actor ID")
        }
    }
}
