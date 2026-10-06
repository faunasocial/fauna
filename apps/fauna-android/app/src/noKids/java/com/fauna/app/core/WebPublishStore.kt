package com.fauna.app.core

import com.fauna.ffi.webPostPageUrl
import com.fauna.ffi.webSiteLinkView
import com.fauna.ffi.webTokenedUrl
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_web.PaywallTarget
import uniffi.fauna_client_web.SiteLinkDisabledReason
import uniffi.fauna_client_web.WebDomainRow
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Shared state for the own-post web-publishing verbs
 * (`web-content-hosting.md` § Published-post management) — the origin every
 * copy affordance resolves against, and the last-copied confirmation — read
 * and written by BOTH `WebSettingsVM` (the `web-settings` Published-posts
 * section) and `FeedVM` (the feed ⋯-menu), so the two surfaces can never
 * disagree about a creator's address. The android twin of linux's
 * `crate::settings::web` thread-local cache / web's `$lib/web-publish`
 * Svelte store.
 */
@Singleton
class WebPublishStore @Inject constructor(
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
    accountStores: AccountStores,
) {
    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())

    private val _origin = MutableStateFlow<String?>(null)

    /** The resolved origin, or `null` until either surface has read it once
     *  this session. */
    val origin: StateFlow<String?> = _origin.asStateFlow()

    private val _disabledReason = MutableStateFlow<SiteLinkDisabledReason?>(null)

    /** Set exactly when [origin] is `null` — why the actor's content has no
     *  public origin, for the copy affordances' explanatory line. */
    val disabledReason: StateFlow<SiteLinkDisabledReason?> = _disabledReason.asStateFlow()

    private val _error = MutableStateFlow<Pair<String, String>?>(null)

    /** `(kind, message)` for the last publish/unpublish/paywall-mint failure —
     *  `kind` is `"publish"` / `"unpublish"` / `"paywall"`, so the screen
     *  applies the right `web_publish_error_*` template (mirrors
     *  `FeedVM.deleteError`/`trainError`'s separate-channel-per-op shape). */
    val error: StateFlow<Pair<String, String>?> = _error.asStateFlow()

    private val _copied = MutableStateFlow<Triple<String, String, String>?>(null)

    /** `(postId, kind, url)` for the last web/paywall link copied — `kind` is
     *  `"web"` or `"paywall"`. */
    val copied: StateFlow<Triple<String, String, String>?> = _copied.asStateFlow()

    private var hydrating = false

    /** Bumped by the account-switch closer, captured at the top of
     *  [ensureOrigin], and re-checked before the read's landing writes
     *  [applyWebPage] or clears [hydrating] — the identity seam ahead of a
     *  detached read (`account-scoping.md` § The scoping taxonomy → the
     *  in-memory corollary; the [com.fauna.app.core.events.EventDraftsHost]
     *  `generation` twin). A stale landing must not clear the INCOMING
     *  actor's in-flight latch, which the closer may have already reset for
     *  a hydrate B itself started. */
    private var generation = 0L

    init {
        accountStores.registerCloser("web-publish") {
            _origin.value = null
            _disabledReason.value = null
            _copied.value = null
            _error.value = null
            hydrating = false
            generation += 1
        }
    }

    /** Populate [origin] from a `web-settings`-page-shaped read (own hydrate,
     *  an echoed toggle flip, or [ensureOrigin]'s own lazy fetch). */
    fun applyWebPage(
        domains: List<WebDomainRow>,
        subdomainEnabled: Boolean,
        handle: String?,
        servingDomain: String,
    ) {
        val link = runCatching {
            webSiteLinkView(domains, subdomainEnabled, handle, servingDomain)
        }.getOrNull()
        _origin.value = link?.origin
        _disabledReason.value = link?.disabledReason
    }

    /** Ensure [origin] is resolved, for a viewer who opens an own post's ⋯
     *  menu without ever having visited Settings → Web this session. No-op
     *  once resolved or already in flight (mirrors linux/web's lazy hydrate
     *  guard) — fire-and-forget, since the caller repaints off [origin]
     *  itself once it lands. */
    fun ensureOrigin() {
        if (_origin.value != null || hydrating) return
        val w = api.webClient() ?: return
        hydrating = true
        val mine = generation
        scope.launch {
            runCatching {
                val handle = secureStorage.handle?.substringBefore("@")?.ifBlank { null }
                val servingDomain = w.servingDomain()
                val enabled = w.getSubdomainEnabled()
                val domains = w.domainGet()
                // A stale read from a torn-down actor must not paint the
                // incoming actor's origin (`account-scoping.md` § The scoping
                // taxonomy → the in-memory corollary).
                if (generation == mine) applyWebPage(domains, enabled, handle, servingDomain)
            }
            // Same guard on the latch: the closer already reset it for
            // whichever actor is current, and a stale landing must not clear
            // an in-flight read the incoming actor itself started.
            if (generation == mine) hydrating = false
        }
    }

    /** `fauna.web.publish.set` for an own unpublished post (default slug) —
     *  suspends until the mutation settles, so the caller's own re-read runs
     *  only after. */
    suspend fun publish(postId: String) {
        val w = api.webClient() ?: return
        runCatching { w.publishSet(HexUtil.hexToBytes(postId), null) }
            .onFailure { _error.value = "publish" to (it.message ?: "publish failed") }
    }

    /** `fauna.web.publish.unset` — idempotent nest-side, hence a one-tap verb
     *  with no confirm step. */
    suspend fun unpublish(postId: String) {
        val w = api.webClient() ?: return
        runCatching { w.publishUnset(HexUtil.hexToBytes(postId)) }
            .onFailure { _error.value = "unpublish" to (it.message ?: "unpublish failed") }
    }

    /** Purely local — the origin and the slug are both already resolved on
     *  screen, so the public link costs no round trip. */
    fun copyWebLink(postId: String, slug: String) {
        val origin = _origin.value ?: return
        _copied.value = Triple(postId, "web", webPostPageUrl(origin, slug))
    }

    /** A fresh mint per click: the token is short-lived by ratified design
     *  and re-minting is free, so re-copying always yields a link that works
     *  from now rather than a cached one that already expired. */
    fun copyPaywallLink(postId: String, slug: String) {
        val w = api.webClient() ?: return
        val origin = _origin.value ?: return
        scope.launch {
            runCatching {
                val minted = w.paywallMintToken(PaywallTarget.PostSlug(slug))
                webTokenedUrl(origin, minted.path, minted.token)
            }.onSuccess { url -> _copied.value = Triple(postId, "paywall", url) }
                .onFailure { _error.value = "paywall" to (it.message ?: "mint paywall link failed") }
        }
    }
}
