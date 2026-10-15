package com.fauna.app.core.feed

import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiFeedManager
import com.fauna.ffi.detectMediaC2pa

/**
 * Fetch a feed post's media blob and open it for rendering — the android leg of
 * the tier-sealed post-media read (`docs/goal/ui/media.md` § Encryption at rest:
 * one per-post key seals a restricted post's body and all its attachments
 * together, so a reader who can open the body can open the attachments by
 * construction).
 *
 * **Every post-image fetch goes through here, gated post or not.** A public
 * post's blob is plaintext on the wire and comes straight back; a tier-restricted
 * post's attachment is AEAD-sealed under the per-post key its body opened under
 * and is opened by the shared `FfiFeedManager.openMediaBytes`. Routing every hash
 * through that one call is what keeps the post card free of an is-this-post-gated
 * branch (`media.md` § Encryption at rest → *Rendering a sealed attachment*, the
 * apps whose image path holds the bytes), which is why this takes no "is it
 * gated" argument and offers no second entry point. windows draws the same line
 * (`FaunaApp.Core.Helpers.PostMediaOpen`).
 *
 * Split out of [com.fauna.app.ui.viewmodel.FeedVM.fetchBlobBytes] so the routing
 * decision is pinned headlessly; the bitmap decode stays in the composable. The
 * fetch stays app glue — the shared manager is WS-RPC-only and never pulls bulk
 * blob bytes itself, the same division `unlockGatedPost` already draws.
 */
object PostMediaOpen {
    /**
     * GET the blob by [hash], then hand the bytes to the manager.
     *
     * @param manager the session's feed manager, or `null` before one is built. A
     *   `null` manager holds no per-post keys, so there is nothing it could open and
     *   the fetched bytes come back unchanged — the answer it would give for an
     *   unregistered hash.
     * @return the bytes to decode, or `null` when the fetch failed or the blob IS a
     *   sealed item of an unlocked post and did not open — the caller paints its
     *   existing placeholder. Never the ciphertext: AEAD bytes handed to
     *   `BitmapFactory` fail exactly like a corrupt image and hide a real key failure.
     */
    suspend fun fetchAndOpen(api: ApiClient, manager: FfiFeedManager?, hash: String): ByteArray? {
        val fetched = runCatching { api.fetchBlobBytes(hash) }.getOrNull() ?: return null
        if (manager == null) return fetched
        return manager.openMediaBytes(hash, fetched)
    }

    /**
     * The `c2pa-badge` verdict (`media.md` § Encryption at rest → *C2PA provenance*).
     * The `x-c2pa` response header is the uploader's own assertion — a UI hint, not a
     * security boundary — so it is only a pre-filter here: `false` ends the check
     * without fetching anything, and `true` is confirmed against the opened bytes
     * through the shared `detectMediaC2pa` export (bytes only, no MIME). Any failure
     * to fetch or open is "no badge".
     */
    suspend fun verifiedC2pa(api: ApiClient, manager: FfiFeedManager?, hash: String): Boolean {
        if (!runCatching { api.checkBlobC2pa(hash) }.getOrDefault(false)) return false
        val bytes = fetchAndOpen(api, manager, hash) ?: return false
        return detectMediaC2pa(bytes)
    }
}
