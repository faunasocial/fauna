package com.fauna.app.core

import com.fauna.ffi.classifyRecipient
import com.fauna.ffi.findUser
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
        val domain: String? = null,
        /**
         * The peer nest the lookup resolved on — `null` for a home-nest actor.
         * The knock hands it to `fauna.inbox.send` as `recipient_nest_url`;
         * carried from the lookup, never re-derived (`contacts.md` §
         * Implementation status today → *Cross-nest knocks*).
         */
        val nestUrl: String? = null
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
                // One shared lookup (`find_user_across_nests`): a home probe with
                // the typed `@domain` (parts[3]) as the multi-domain qualifier, so
                // a multi-domain handle echoes it and the find-result renders
                // `bob@domain2` (mail-multidomain.md § Multi-domain handles §
                // Resolution); and, when the domain is foreign, a direct dial of
                // the peer. [actorId, handle, domain, nestUrl] — nestUrl is the
                // peer it resolved on, empty for a home-nest actor.
                val found = findUser(api.nodeUrl, parts[2], parts[3])
                val peerNestUrl = found[3].ifEmpty { null }
                ResolvedRecipient(
                    actorId = found[0],
                    nodeUrl = peerNestUrl ?: api.nodeUrl,
                    handle = found[1],
                    domain = found[2],
                    nestUrl = peerNestUrl
                )
            }
            else -> throw IllegalArgumentException("Enter a handle (alice@fauna.social) or 64-char actor ID")
        }
    }
}
